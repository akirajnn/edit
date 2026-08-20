// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Completing a word from the ones already in the file.
//!
//! No language server, no subprocess, no configuration: the words you want to
//! type are, almost always, words this file already contains. Add the language
//! keywords from `completion.keywords` and that covers the rest.
//!
//! # Only when asked
//!
//! The list appears when asked for and never on its own, which is what keeps
//! it out of the way of ordinary typing. One pair of keys both opens it and
//! walks through it: press next once for the list, again for the entry after
//! that. Prev opens it on the *last* entry, so reaching for the bottom of a
//! short list is one keystroke either way. This is how vim's completion works
//! and it means the fingers never have to move to the arrow keys.
//!
//! Which keys those are is a setting, because the obvious ones are not always
//! available -- see [`crate::keybind`]. Up, down, enter, tab and escape work
//! as well while the list is open; every other keystroke goes through to the
//! buffer untouched.
//!
//! That is why this doesn't fight the text area for the input focus, which is
//! the thing that makes completion popups awkward in an immediate-mode UI. The
//! popup never takes focus at all. Typing goes to the buffer as usual, and
//! afterwards [`draw_completion`] re-reads the word under the cursor and
//! narrows the list to match. The list follows the buffer rather than
//! intercepting the way there.

use std::ops::Range;

use edit::buffer::TextBuffer;
use edit::fuzzy::score_fuzzy;
use edit::helpers::*;
use edit::input::vk;
use edit::tui::*;
use stdext::arena::scratch_arena;

use crate::settings::Settings;
use crate::state::State;

/// Most words harvested from a document.
///
/// Reached only by generated or minified files, where the tail of the list is
/// noise anyway. It bounds both the memory and the ranking work.
const MAX_CANDIDATES: usize = 20_000;

/// Most entries shown at once.
///
/// The list scrolls, but nobody reads to the bottom of a hundred guesses, and
/// every entry past this one is layout work for something unseen.
const MAX_VISIBLE: usize = 40;

/// How tall the popup is allowed to get.
const MAX_ROWS: CoordType = 10;

/// An open completion list.
pub struct Completion {
    /// Byte offset where the word being completed starts.
    ///
    /// The end is wherever the cursor is now, so the list narrows as the user
    /// keeps typing without anything having to track the keystrokes.
    start: usize,
    /// Every word available, unranked. Gathered once when the list opens.
    candidates: Vec<String>,
    /// Indices into `candidates`, best first, for the current prefix.
    matches: Vec<usize>,
    /// The prefix `matches` was computed for, so it is only redone on change.
    needle: String,
    selected: usize,
}

impl Completion {
    fn selection(&self) -> Option<&str> {
        self.matches.get(self.selected).map(|&i| self.candidates[i].as_str())
    }

    /// Moves the highlight, wrapping at both ends.
    ///
    /// Wrapping because a short list is quicker to cycle through than to
    /// reverse direction in, and these lists are always short.
    fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }
}

/// Claims the keys the list needs, before the text area sees them.
///
/// Runs at the very top of the frame for the same reason the terminal panel's
/// shortcuts do: whatever isn't claimed here is about to be swallowed by the
/// control that has the focus.
pub fn draw_completion_shortcuts(ctx: &mut Context, state: &mut State) {
    let (next, prev) = {
        let settings = Settings::borrow();
        (settings.completion_next, settings.completion_prev)
    };

    if state.completion.is_some() {
        if ctx.consume_shortcut(vk::ESCAPE) {
            state.completion = None;
            ctx.needs_rerender();
            return;
        }
        if ctx.consume_shortcut(vk::UP) || prev.is_some_and(|k| ctx.consume_shortcut(k)) {
            move_selection(state, -1);
            ctx.needs_rerender();
            return;
        }
        if ctx.consume_shortcut(vk::DOWN) || next.is_some_and(|k| ctx.consume_shortcut(k)) {
            move_selection(state, 1);
            ctx.needs_rerender();
            return;
        }
        if ctx.consume_shortcut(vk::RETURN) || ctx.consume_shortcut(vk::TAB) {
            accept(state);
            ctx.needs_rerender();
            return;
        }
        return;
    }

    // Closed: either key opens it. Prev starts at the bottom, so the last
    // entry is one keystroke away just like the first.
    if next.is_some_and(|k| ctx.consume_shortcut(k)) {
        state.wants_completion = Some(Opening::First);
        ctx.needs_rerender();
    } else if prev.is_some_and(|k| ctx.consume_shortcut(k)) {
        state.wants_completion = Some(Opening::Last);
        ctx.needs_rerender();
    }
}

/// Which end of the list a freshly opened one starts at.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    First,
    Last,
}

/// Opens, narrows, or closes the list, and draws it.
///
/// Must be called straight after the text area, because the popup is anchored
/// to it -- see [`Context::textarea_cursor_offset`].
pub fn draw_completion(ctx: &mut Context, state: &mut State) {
    if let Some(opening) = state.wants_completion.take() {
        open(state, opening);
    }

    if state.completion.is_none() {
        return;
    }

    // The buffer has already handled this frame's keystroke, so the word under
    // the cursor is the current one. Re-reading it is how the list follows
    // along without intercepting anything.
    if !narrow(state) {
        state.completion = None;
        return;
    }

    let Some(cursor) = ctx.textarea_cursor_offset() else {
        state.completion = None;
        return;
    };

    render(ctx, state, cursor);
}

/// Gathers the candidates and shows the list, if there is anything to show.
fn open(state: &mut State, opening: Opening) {
    state.completion = None;

    let Some(doc) = state.documents.active() else {
        return;
    };

    let (start, needle, candidates) = {
        let tb = doc.buffer.borrow();

        // With no word under the cursor, complete from the empty prefix --
        // every word in the file. Doing nothing instead would be
        // indistinguishable from the key not having arrived, and "show me
        // what's here" is a reasonable thing to have asked for anyway.
        let cursor = tb.cursor_offset();
        let word = word_before_cursor(&tb).unwrap_or(cursor..cursor);

        let language = tb.language().map(|l| l.id);
        // Nothing to skip when nothing is being typed; a word starting exactly
        // at the cursor is a word *after* it, and still worth offering.
        let skip = if word.is_empty() { None } else { Some(word.start) };

        let candidates = harvest(&tb, language, skip);
        let needle = read_range(&tb, word.clone());
        (word.start, needle, candidates)
    };

    let mut completion =
        Completion { start, candidates, matches: Vec::new(), needle: String::new(), selected: 0 };

    rank(&mut completion, &needle);
    if completion.matches.is_empty() {
        return;
    }

    if opening == Opening::Last {
        completion.selected = completion.matches.len() - 1;
    }

    state.completion = Some(completion);
}

/// Re-reads the prefix and re-ranks if it changed.
///
/// Returns false when the cursor has left the word the list was opened for,
/// which is the signal to close.
fn narrow(state: &mut State) -> bool {
    let Some(doc) = state.documents.active() else {
        return false;
    };
    let Some(completion) = state.completion.as_mut() else {
        return false;
    };

    let tb = doc.buffer.borrow();
    let cursor = tb.cursor_offset();

    // Moving before the start, or off the end of the word, ends the session.
    if cursor < completion.start {
        return false;
    }

    let needle = read_range(&tb, completion.start..cursor);
    if !needle.chars().all(is_word_char) {
        return false;
    }

    if needle != completion.needle {
        rank(completion, &needle);
    }

    !completion.matches.is_empty()
}

/// Scores every candidate against the prefix and keeps the best.
fn rank(completion: &mut Completion, needle: &str) {
    let scratch = scratch_arena(None);
    let mut scored: Vec<(i32, usize)> = Vec::new();

    for (index, candidate) in completion.candidates.iter().enumerate() {
        // With nothing typed yet, everything is a candidate and the order is
        // whatever the document gave us, which is near enough to "nearest
        // first" to be useful.
        if needle.is_empty() {
            scored.push((0, index));
            continue;
        }

        // A word can't complete a prefix longer than itself, and skipping
        // those before scoring avoids the expensive part for most of them.
        if candidate.len() < needle.len() {
            continue;
        }

        let (score, _) = score_fuzzy(&scratch, candidate, needle, true);
        if score > 0 {
            scored.push((score, index));
        }
    }

    // Best first; ties keep the order the document had them in, which is
    // stable and therefore doesn't shuffle as you type.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(MAX_VISIBLE);

    completion.matches = scored.into_iter().map(|(_, index)| index).collect();
    completion.needle = needle.to_string();
    completion.selected = 0;
}

fn move_selection(state: &mut State, delta: isize) {
    if let Some(completion) = state.completion.as_mut() {
        completion.move_selection(delta);
    }
}

/// Replaces the typed prefix with the highlighted word.
fn accept(state: &mut State) {
    let Some(completion) = state.completion.take() else {
        return;
    };
    let Some(word) = completion.selection().map(str::to_string) else {
        return;
    };
    let Some(doc) = state.documents.active() else {
        return;
    };

    let mut tb = doc.buffer.borrow_mut();
    let end = tb.cursor_offset();
    if end < completion.start {
        return;
    }

    // Select what was typed and write over it, so the whole thing lands in the
    // undo history as one step rather than as a delete and an insert.
    tb.cursor_move_to_offset(completion.start);
    tb.start_selection();
    tb.selection_update_offset(end);
    tb.write_raw(word.as_bytes());
    tb.make_cursor_visible();
}

/// Draws the list, anchored under the cursor.
fn render(ctx: &mut Context, state: &mut State, cursor: Point) {
    let Some(completion) = state.completion.as_mut() else {
        return;
    };

    let rows = (completion.matches.len() as CoordType).min(MAX_ROWS);
    // Above the cursor when there isn't room below, so the list never covers
    // the line being typed on.
    let flip = cursor.y + 1 + rows + 2 > ctx.size().height - 2;

    let mut chosen = None;

    ctx.list_begin("completion");
    ctx.attr_float(FloatSpec {
        anchor: Anchor::Last,
        gravity_x: 0.0,
        // Flipping the box's own origin makes an upward list grow from its
        // bottom edge instead of being drawn off the top of the screen.
        gravity_y: if flip { 1.0 } else { 0.0 },
        offset_x: cursor.x as f32,
        offset_y: if flip { cursor.y as f32 } else { (cursor.y + 1) as f32 },
    });
    ctx.attr_border();
    {
        for (row, &index) in completion.matches.iter().enumerate() {
            ctx.next_block_id_mixin(row as u64);
            if ctx.list_item(row == completion.selected, &completion.candidates[index])
                != ListSelection::Unchanged
            {
                // Clicking an entry picks it; the keyboard path goes through
                // the shortcut handler instead.
                chosen = Some(row);
            }
        }
    }
    ctx.list_end();

    if let Some(row) = chosen {
        if let Some(completion) = state.completion.as_mut() {
            completion.selected = row;
        }
        accept(state);
        ctx.needs_rerender();
    }
}

/// Whether a character can be part of a word being completed.
///
/// Deliberately includes non-ASCII letters: `漢字` is a perfectly good Rust
/// identifier, and an editor that refused to complete it would be wrong in a
/// way that only shows up for some people.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// The word the cursor is sitting at the end of.
///
/// `None` when the cursor isn't after a word character, because completing
/// nothing would mean offering the entire document.
fn word_before_cursor(tb: &TextBuffer) -> Option<Range<usize>> {
    let cursor = tb.cursor_offset();
    let before = tb.as_document().read_backward(cursor);

    // A gap buffer hands back at most two runs and this only ever needs the
    // tail of one word, so looking at the last chunk is enough unless a word
    // straddles the gap -- in which case the prefix is merely shorter than it
    // could be, and the list is a little wider. Not worth stitching for.
    let text = String::from_utf8_lossy(before);
    let mut start = text.len();

    for (at, ch) in text.char_indices().rev() {
        if !is_word_char(ch) {
            break;
        }
        start = at;
    }

    if start == text.len() {
        return None;
    }

    Some(cursor - (text.len() - start)..cursor)
}

fn read_range(tb: &TextBuffer, range: Range<usize>) -> String {
    let document = tb.as_document();
    let mut out = Vec::with_capacity(range.len());
    let mut at = range.start;

    while at < range.end {
        let chunk = document.read_forward(at);
        if chunk.is_empty() {
            break;
        }
        let take = chunk.len().min(range.end - at);
        out.extend_from_slice(&chunk[..take]);
        at += take;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// Every distinct word in the document, plus the language's keywords.
///
/// `skip` is where the occurrence being typed starts, which would otherwise
/// offer itself as a completion of itself. `None` skips nothing.
///
/// An `Option` rather than an empty range, because a range needs a position to
/// be empty *at*, and `0..0` would silently swallow the first word in the file.
fn harvest(tb: &TextBuffer, language: Option<&str>, skip: Option<usize>) -> Vec<String> {
    let document = tb.as_document();
    let mut text = Vec::with_capacity(tb.text_length());
    let mut at = 0;

    loop {
        let chunk = document.read_forward(at);
        if chunk.is_empty() {
            break;
        }
        text.extend_from_slice(chunk);
        at += chunk.len();
    }

    let text = String::from_utf8_lossy(&text);
    let mut words: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    let mut word_start = None;
    for (at, ch) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        match (is_word_char(ch), word_start) {
            (true, None) => word_start = Some(at),
            (false, Some(start)) => {
                word_start = None;

                // The word being typed is not a completion of itself.
                if Some(start) == skip {
                    continue;
                }

                let word = &text[start..at];
                // A single character is never worth suggesting, and numbers
                // are not words anyone completes.
                if word.chars().count() < 2 || word.chars().next().is_some_and(|c| c.is_numeric()) {
                    continue;
                }

                if !seen.iter().any(|s| s == word) {
                    seen.push(word.to_string());
                    words.push(word.to_string());
                }

                if words.len() >= MAX_CANDIDATES {
                    break;
                }
            }
            _ => {}
        }
    }

    if let Some(language) = language {
        for keyword in Settings::borrow().completion_keywords(language) {
            if !seen.iter().any(|s| s == keyword) {
                seen.push(keyword.to_string());
                words.push(keyword.to_string());
            }
        }
    }

    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> TextBuffer {
        let mut tb = TextBuffer::new(true).unwrap();
        tb.set_crlf(false);
        tb.write_raw(text.as_bytes());
        tb
    }

    /// Puts the cursor at the first `|` in `text`, which is then removed.
    fn buffer_at(text: &str) -> TextBuffer {
        let at = text.find('|').expect("mark the cursor with |");
        let mut tb = buffer(&text.replace('|', ""));
        tb.cursor_move_to_offset(at);
        tb
    }

    #[test]
    fn finds_the_word_the_cursor_is_in() {
        let tb = buffer_at("let price = 1;\nlet pri|");
        let range = word_before_cursor(&tb).unwrap();
        assert_eq!(read_range(&tb, range), "pri");
    }

    #[test]
    fn there_is_no_word_after_a_separator() {
        // The caller turns this into "complete from the empty prefix", i.e.
        // offer everything, rather than doing nothing.
        assert!(word_before_cursor(&buffer_at("let x = |")).is_none());
        assert!(word_before_cursor(&buffer_at("|")).is_none());
    }

    #[test]
    fn a_word_can_be_non_ascii() {
        let tb = buffer_at("let 漢字 = 1;\n漢|");
        let range = word_before_cursor(&tb).unwrap();
        assert_eq!(read_range(&tb, range), "漢");
    }

    #[test]
    fn harvests_distinct_words_only() {
        let tb = buffer("alpha beta alpha gamma beta");
        let words = harvest(&tb, None, None);
        assert_eq!(words, ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn skips_single_characters_and_numbers() {
        // `x` is too short to be worth offering, and nobody completes `42`.
        let tb = buffer("let x = 42; let count = 1;");
        let words = harvest(&tb, None, None);
        assert_eq!(words, ["let", "count"]);
    }

    #[test]
    fn the_word_being_typed_does_not_complete_itself() {
        // "pri" at the end is what the user is typing; "price" earlier is a
        // genuine candidate, but the "pri" occurrence itself is not.
        let tb = buffer_at("price = 1;\npri|");
        let skip = word_before_cursor(&tb).unwrap();
        let words = harvest(&tb, None, Some(skip.start));
        assert_eq!(words, ["price"]);
    }

    #[test]
    fn splits_words_on_punctuation() {
        let tb = buffer("self.count += other.count;");
        let words = harvest(&tb, None, None);
        assert_eq!(words, ["self", "count", "other"]);
    }

    #[test]
    fn ranks_the_closest_match_first() {
        let mut completion = Completion {
            start: 0,
            candidates: vec![
                "printer".to_string(),
                "println".to_string(),
                "pathological_rich_input".to_string(),
            ],
            matches: Vec::new(),
            needle: String::new(),
            selected: 0,
        };

        rank(&mut completion, "pri");

        // All three contain p, r, i in order, but the two that start with the
        // prefix must come first.
        let ranked: Vec<&str> =
            completion.matches.iter().map(|&i| completion.candidates[i].as_str()).collect();
        assert!(
            ranked[0].starts_with("pri") && ranked[1].starts_with("pri"),
            "prefix matches should win: {ranked:?}"
        );
    }

    #[test]
    fn a_prefix_longer_than_a_word_cannot_match_it() {
        let mut completion = Completion {
            start: 0,
            candidates: vec!["ab".to_string()],
            matches: Vec::new(),
            needle: String::new(),
            selected: 0,
        };

        rank(&mut completion, "abcdef");
        assert!(completion.matches.is_empty());
    }

    #[test]
    fn an_empty_prefix_offers_everything() {
        let mut completion = Completion {
            start: 0,
            candidates: vec!["one".to_string(), "two".to_string()],
            matches: Vec::new(),
            needle: String::new(),
            selected: 0,
        };

        rank(&mut completion, "");
        assert_eq!(completion.matches.len(), 2);
    }

    #[test]
    fn the_selection_wraps_in_both_directions() {
        let mut completion = Completion {
            start: 0,
            candidates: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            matches: vec![0, 1, 2],
            needle: String::new(),
            selected: 0,
        };

        completion.move_selection(1);
        assert_eq!(completion.selection(), Some("b"));

        // Past the end comes back to the top...
        completion.move_selection(1);
        completion.move_selection(1);
        assert_eq!(completion.selection(), Some("a"));

        // ...and before the start goes to the bottom.
        completion.move_selection(-1);
        assert_eq!(completion.selection(), Some("c"));
    }

    #[test]
    fn moving_an_empty_selection_does_nothing() {
        let mut completion = Completion {
            start: 0,
            candidates: Vec::new(),
            matches: Vec::new(),
            needle: String::new(),
            selected: 0,
        };

        // `rem_euclid` by zero would panic, so the guard matters.
        completion.move_selection(1);
        assert_eq!(completion.selection(), None);
    }
}
