// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use lsh::runtime::*;
use stdext::arena::{Arena, scratch_arena};
use stdext::collections::BVec;

use crate::document::ReadableDocument;
use crate::helpers::*;
use crate::lsh::definitions::*;
use crate::{simd, unicode};

const MAX_LINE_LEN: usize = 32 * KIBI;

#[derive(Clone)]
pub struct Highlighter<'a> {
    doc: &'a dyn ReadableDocument,
    offset: usize,
    logical_pos_y: CoordType,
    runtime: Runtime<'static, 'static, 'static>,
}

#[derive(Clone)]
pub struct HighlighterState {
    offset: usize,
    logical_pos_y: CoordType,
    state: RuntimeState,
}

impl<'doc> Highlighter<'doc> {
    pub fn new(doc: &'doc dyn ReadableDocument, language: &'static Language) -> Self {
        Self {
            doc,
            offset: 0,
            logical_pos_y: 0,
            runtime: Runtime::new(&ASSEMBLY, &STRINGS, &CHARSETS, language.entrypoint),
        }
    }

    pub fn logical_pos_y(&self) -> CoordType {
        self.logical_pos_y
    }

    /// Create a restorable snapshot of the current highlighter state
    /// so we can resume highlighting from this point later.
    pub fn snapshot(&self) -> HighlighterState {
        HighlighterState {
            offset: self.offset,
            logical_pos_y: self.logical_pos_y,
            state: self.runtime.snapshot(),
        }
    }

    /// Restore the highlighter state from a previously captured snapshot.
    pub fn restore(&mut self, snapshot: &HighlighterState) {
        self.offset = snapshot.offset;
        self.logical_pos_y = snapshot.logical_pos_y;
        self.runtime.restore(&snapshot.state);
    }

    pub fn parse_next_line<'a>(&mut self, arena: &'a Arena) -> BVec<'a, Highlight<HighlightKind>> {
        let scratch = scratch_arena(Some(arena));
        let (line_off, line) = self.read_next_line(&scratch);

        // Empty lines can be somewhat common.
        //
        // If the line is too long, we don't highlight it.
        // This is to prevent performance issues with very long lines.
        if line.is_empty() || line.len() >= MAX_LINE_LEN {
            return BVec::empty();
        }

        let line = unicode::strip_newline(line);
        let mut res = self.runtime.parse_next_line(arena, line);

        // Adjust the range to account for the line offset.
        for h in res.iter_mut() {
            h.start = line_off + h.start.min(line.len());
        }

        res
    }

    fn read_next_line<'a>(&mut self, arena: &'a Arena) -> (usize, &'a [u8])
    where
        'doc: 'a,
    {
        self.logical_pos_y += 1;

        let line_beg = self.offset;
        let mut chunk;
        let mut line_buf;

        // Try to read a chunk and see if it contains a newline.
        // In that case we can skip concatenating chunks.
        {
            chunk = self.doc.read_forward(self.offset);
            if chunk.is_empty() {
                return (line_beg, chunk);
            }

            let (off, line) = simd::lines_fwd(chunk, 0, 0, 1);
            self.offset += off;

            if line == 1 {
                return (line_beg, &chunk[..off]);
            }

            let next_chunk = self.doc.read_forward(self.offset);
            if next_chunk.is_empty() {
                return (line_beg, &chunk[..off]);
            }

            line_buf = BVec::empty();

            // Ensure we don't overflow the heap size with a 1GB long line.
            let end = off.min(MAX_LINE_LEN - line_buf.len());
            let end = end.min(chunk.len());
            line_buf.extend_from_slice(arena, &chunk[..end]);

            chunk = next_chunk;
        }

        // Concatenate chunks until we get a full line.
        while line_buf.len() < MAX_LINE_LEN {
            let (off, line) = simd::lines_fwd(chunk, 0, 0, 1);
            self.offset += off;

            // Ensure we don't overflow the heap size with a 1GB long line.
            let end = off.min(MAX_LINE_LEN - line_buf.len());
            let end = end.min(chunk.len());
            line_buf.extend_from_slice(arena, &chunk[..end]);

            // Start of the next line found.
            if line == 1 {
                break;
            }

            chunk = self.doc.read_forward(self.offset);
            if chunk.is_empty() {
                break;
            }
        }

        (line_beg, line_buf.leak())
    }
}

#[cfg(test)]
mod tests {
    use stdext::arena::scratch_arena;

    use super::*;
    use crate::lsh::LANGUAGES;

    fn language(id: &str) -> &'static Language {
        LANGUAGES.iter().find(|l| l.id == id).unwrap_or_else(|| panic!("no language {id:?}"))
    }

    /// Returns the highlight kind in effect at the *start* of each line.
    ///
    /// That's the signal for whether a multi-line construct on the previous
    /// line was closed: a line that opens still inside a comment is one the
    /// comment leaked into.
    fn kind_at_line_start(source: &str, language_id: &str) -> Vec<HighlightKind> {
        let bytes = source.as_bytes();
        let doc: &dyn ReadableDocument = &bytes;
        let mut highlighter = Highlighter::new(doc, language(language_id));
        let mut out = Vec::new();
        let mut line_start = 0usize;

        for line in source.lines() {
            let arena = scratch_arena(None);
            let highlights = highlighter.parse_next_line(&arena);

            // Highlights carry absolute offsets and are sorted, so the kind at
            // a position is the last one that starts at or before it. Several
            // may share an offset; the last of those wins.
            let kind = highlights
                .iter()
                .take_while(|h| h.start <= line_start)
                .last()
                .map_or(HighlightKind::Other, |h| h.kind);

            out.push(kind);
            line_start += line.len() + 1;
        }

        out
    }

    /// A block comment that ends at the very end of a line used to swallow the
    /// rest of the file: the loop's "did this iteration make progress" check
    /// compared against an offset saved before `await input`, which is stale
    /// once we're on a new line. When the closing `*/` happened to sit at that
    /// same column, the built-in advance step skipped over it.
    ///
    /// That made it depend on the *length* of the comment, so these cases pair
    /// an 8 character first line with a `*/` at column 8.
    #[test]
    fn block_comment_ending_a_line_does_not_leak() {
        for language_id in ["rust", "javascript", "c", "cpp", "csharp", "java"] {
            let kinds = kind_at_line_start("/* multi\n   line */\nnot_a_comment\n", language_id);

            // Line 2 is inside the comment, which is the whole point of the setup.
            assert_eq!(kinds[1], HighlightKind::Comment, "{language_id} line 2");
            assert_ne!(
                kinds[2],
                HighlightKind::Comment,
                "{language_id}: the comment leaked past its closing `*/`",
            );
        }
    }

    /// The same shape, but with the comment closing mid-line. This always
    /// worked and is here to keep the fix from breaking it.
    #[test]
    fn block_comment_ending_mid_line_does_not_leak() {
        let kinds = kind_at_line_start("/* multi\n   line */ x\nnot_a_comment\n", "rust");
        assert_eq!(kinds[1], HighlightKind::Comment);
        assert_ne!(kinds[2], HighlightKind::Comment);
    }

    /// Single line block comments never went through the suspend/resume path.
    #[test]
    fn single_line_block_comment_does_not_leak() {
        let kinds = kind_at_line_start("/* one */\nnot_a_comment\n", "rust");
        assert_ne!(kinds[1], HighlightKind::Comment);
    }

    /// The leak depended on the closing `*/` lining up with a stale offset, so
    /// sweep the alignment rather than trusting one hand-picked case.
    #[test]
    fn block_comment_closes_at_every_alignment() {
        for body in 1..24usize {
            for indent in 0..12usize {
                let source = format!(
                    "/*{}\n{}*/\nnot_a_comment\n",
                    "x".repeat(body),
                    " ".repeat(indent)
                );
                let kinds = kind_at_line_start(&source, "rust");
                assert_ne!(
                    kinds[2],
                    HighlightKind::Comment,
                    "leaked with a {body} character body and {indent} spaces of indent",
                );
            }
        }
    }
}
