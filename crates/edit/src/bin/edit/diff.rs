// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! A line diff, used to show what changed before reloading a file.
//!
//! This exists to answer one question for the user: "if I reload, what do I
//! gain and what do I lose?" Nothing consumes the output programmatically, so
//! it stays line based and gives up on inputs where a detailed answer would
//! stop being useful anyway.

/// Beyond this the file isn't something anyone reads a diff of.
const MAX_LINES: usize = 50_000;

/// Cap on the comparison table, which is what bounds the memory this can use.
/// Reached only when the two sides differ over a long stretch, since the
/// shared head and tail are removed first.
const MAX_TABLE: usize = 1_000_000;

/// Unchanged lines kept on each side of a change.
const CONTEXT: usize = 3;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DiffLine {
    Context(String),
    Removed(String),
    Added(String),
    /// Unchanged lines elided between two changes.
    Skipped(usize),
}

#[derive(PartialEq, Eq, Debug)]
pub enum DiffResult {
    /// No differences at all.
    Identical,
    Lines(Vec<DiffLine>),
    /// Too different to be worth showing line by line.
    Summary { removed: usize, added: usize },
    /// Too big to diff.
    TooLarge,
}

/// Compares two texts by line.
///
/// `mine` is what the editor holds and `theirs` is what's on disk, so a
/// [`DiffLine::Removed`] line is one that reloading would take away.
pub fn diff_lines(mine: &str, theirs: &str) -> DiffResult {
    let a: Vec<&str> = mine.lines().collect();
    let b: Vec<&str> = theirs.lines().collect();

    if a.len() > MAX_LINES || b.len() > MAX_LINES {
        return DiffResult::TooLarge;
    }

    // Edits are nearly always local, so removing the shared head and tail
    // usually leaves a handful of lines for the expensive part to look at.
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }

    let mut tail = 0;
    while tail < a.len() - head
        && tail < b.len() - head
        && a[a.len() - 1 - tail] == b[b.len() - 1 - tail]
    {
        tail += 1;
    }

    let a_mid = &a[head..a.len() - tail];
    let b_mid = &b[head..b.len() - tail];

    if a_mid.is_empty() && b_mid.is_empty() {
        return DiffResult::Identical;
    }

    // One side being empty is a pure insertion or deletion; no table needed.
    let script = if a_mid.is_empty() {
        b_mid.iter().map(|line| Edit::Add(line)).collect()
    } else if b_mid.is_empty() {
        a_mid.iter().map(|line| Edit::Remove(line)).collect()
    } else {
        if (a_mid.len() + 1) * (b_mid.len() + 1) > MAX_TABLE {
            return DiffResult::Summary { removed: a_mid.len(), added: b_mid.len() };
        }
        edit_script(a_mid, b_mid)
    };

    let mut lines = Vec::with_capacity(script.len() + head + tail);
    lines.extend(a[..head].iter().map(|l| DiffLine::Context(l.to_string())));
    lines.extend(script.into_iter().map(|edit| match edit {
        Edit::Keep(line) => DiffLine::Context(line.to_string()),
        Edit::Remove(line) => DiffLine::Removed(line.to_string()),
        Edit::Add(line) => DiffLine::Added(line.to_string()),
    }));
    lines.extend(a[a.len() - tail..].iter().map(|l| DiffLine::Context(l.to_string())));

    DiffResult::Lines(collapse(lines))
}

enum Edit<'a> {
    Keep(&'a str),
    Remove(&'a str),
    Add(&'a str),
}

/// Longest common subsequence, then a walk back through it.
///
/// `table[i][j]` is the length of the longest common subsequence of `a[i..]`
/// and `b[j..]`, so the walk can start at the front and always knows which way
/// to go.
fn edit_script<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<Edit<'a>> {
    let n = a.len();
    let m = b.len();
    let stride = m + 1;
    let mut table = vec![0u32; (n + 1) * stride];

    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * stride + j] = if a[i] == b[j] {
                table[(i + 1) * stride + j + 1] + 1
            } else {
                table[(i + 1) * stride + j].max(table[i * stride + j + 1])
            };
        }
    }

    let mut edits = Vec::new();
    let (mut i, mut j) = (0, 0);

    while i < n && j < m {
        if a[i] == b[j] {
            edits.push(Edit::Keep(a[i]));
            i += 1;
            j += 1;
        } else if table[(i + 1) * stride + j] >= table[i * stride + j + 1] {
            edits.push(Edit::Remove(a[i]));
            i += 1;
        } else {
            edits.push(Edit::Add(b[j]));
            j += 1;
        }
    }

    edits.extend(a[i..].iter().map(|line| Edit::Remove(line)));
    edits.extend(b[j..].iter().map(|line| Edit::Add(line)));
    edits
}

/// Replaces runs of unchanged lines further than [`CONTEXT`] from any change
/// with a single marker.
fn collapse(lines: Vec<DiffLine>) -> Vec<DiffLine> {
    let changed: Vec<bool> = lines.iter().map(|l| !matches!(l, DiffLine::Context(_))).collect();

    let mut out = Vec::new();
    let mut skipped = 0;

    for (i, line) in lines.into_iter().enumerate() {
        let lo = i.saturating_sub(CONTEXT);
        let hi = (i + CONTEXT + 1).min(changed.len());
        let near_a_change = changed[lo..hi].iter().any(|c| *c);

        if near_a_change {
            if skipped > 0 {
                out.push(DiffLine::Skipped(skipped));
                skipped = 0;
            }
            out.push(line);
        } else {
            skipped += 1;
        }
    }

    if skipped > 0 {
        out.push(DiffLine::Skipped(skipped));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(result: &DiffResult) -> &[DiffLine] {
        match result {
            DiffResult::Lines(lines) => lines,
            other => panic!("expected a line diff, got {other:?}"),
        }
    }

    fn removed(result: &DiffResult) -> Vec<String> {
        lines(result)
            .iter()
            .filter_map(|l| match l {
                DiffLine::Removed(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    fn added(result: &DiffResult) -> Vec<String> {
        lines(result)
            .iter()
            .filter_map(|l| match l {
                DiffLine::Added(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn identical_input_has_no_diff() {
        assert_eq!(diff_lines("a\nb\nc\n", "a\nb\nc\n"), DiffResult::Identical);
        assert_eq!(diff_lines("", ""), DiffResult::Identical);
    }

    #[test]
    fn pure_addition() {
        let result = diff_lines("a\n", "a\nb\n");
        assert_eq!(added(&result), ["b"]);
        assert!(removed(&result).is_empty());
    }

    #[test]
    fn pure_deletion() {
        let result = diff_lines("a\nb\n", "a\n");
        assert_eq!(removed(&result), ["b"]);
        assert!(added(&result).is_empty());
    }

    #[test]
    fn a_replacement_reports_both_sides() {
        let result = diff_lines("a\nOLD\nc\n", "a\nNEW\nc\n");
        assert_eq!(removed(&result), ["OLD"]);
        assert_eq!(added(&result), ["NEW"]);
    }

    #[test]
    fn a_change_in_the_middle_keeps_its_surroundings() {
        let mine = "1\n2\n3\n4\nOLD\n6\n7\n8\n";
        let theirs = "1\n2\n3\n4\nNEW\n6\n7\n8\n";
        let result = diff_lines(mine, theirs);
        let lines = lines(&result);

        assert!(lines.contains(&DiffLine::Removed("OLD".to_string())));
        assert!(lines.contains(&DiffLine::Context("4".to_string())));
        assert!(lines.contains(&DiffLine::Context("6".to_string())));
    }

    /// The case from the screenshot that prompted this: one line replaced by
    /// several, in the middle of an otherwise untouched file.
    #[test]
    fn one_line_becoming_many() {
        let mine = "a\nb\n* Clone the repository\nc\nd\n";
        let theirs = "a\nb\n* Clone this fork:\n  git clone url\n  cd edit\nc\nd\n";
        let result = diff_lines(mine, theirs);

        assert_eq!(removed(&result), ["* Clone the repository"]);
        assert_eq!(added(&result), ["* Clone this fork:", "  git clone url", "  cd edit"]);
    }

    #[test]
    fn long_unchanged_stretches_are_collapsed() {
        let mut mine = String::from("HEAD\n");
        let mut theirs = String::from("HEAD-CHANGED\n");
        for i in 0..50 {
            mine.push_str(&format!("line{i}\n"));
            theirs.push_str(&format!("line{i}\n"));
        }

        let result = diff_lines(&mine, &theirs);
        let lines = lines(&result);

        assert!(lines.iter().any(|l| matches!(l, DiffLine::Skipped(n) if *n > 20)));
        // The collapsed form must be far shorter than the file.
        assert!(lines.len() < 15, "{lines:?}");
    }

    #[test]
    fn oversized_input_is_rejected_before_diffing() {
        let huge = "x\n".repeat(MAX_LINES + 1);
        assert_eq!(diff_lines(&huge, "y\n"), DiffResult::TooLarge);
    }

    #[test]
    fn a_large_unrelated_rewrite_falls_back_to_a_summary() {
        // No shared lines, so nothing gets trimmed and the table would be
        // enormous.
        let mine: String = (0..2000).map(|i| format!("a{i}\n")).collect();
        let theirs: String = (0..2000).map(|i| format!("b{i}\n")).collect();

        match diff_lines(&mine, &theirs) {
            DiffResult::Summary { removed, added } => {
                assert_eq!((removed, added), (2000, 2000));
            }
            other => panic!("expected a summary, got {other:?}"),
        }
    }

    #[test]
    fn handles_a_missing_trailing_newline() {
        let result = diff_lines("a\nb", "a\nc");
        assert_eq!(removed(&result), ["b"]);
        assert_eq!(added(&result), ["c"]);
    }

    #[test]
    fn handles_an_empty_side() {
        let result = diff_lines("", "a\nb\n");
        assert_eq!(added(&result), ["a", "b"]);

        let result = diff_lines("a\nb\n", "");
        assert_eq!(removed(&result), ["a", "b"]);
    }
}
