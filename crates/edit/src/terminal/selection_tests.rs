// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Tests for selecting and copying text out of the panel.
//!
//! The part worth testing is that a selection stays on the text it was made
//! on. Scrolling the view, and the child printing more output, both move the
//! text under the screen coordinates the user clicked at, and getting that
//! wrong means copying something other than what is highlighted -- a failure
//! nobody would notice until they pasted it somewhere.

use super::screen::*;
use crate::helpers::{CoordType, Size};

const SIZE: Size = Size { width: 10, height: 4 };

fn screen_with_scrollback(limit: usize) -> Screen {
    Screen::new(SIZE, limit)
}

fn write(screen: &mut Screen, text: &str) {
    for ch in text.chars() {
        match ch {
            '\n' => {
                screen.carriage_return();
                screen.line_feed();
            }
            ch => screen.write_char(ch),
        }
    }
}

/// Selects from one screen position to another, as a drag would.
fn select(screen: &mut Screen, from: (CoordType, CoordType), to: (CoordType, CoordType)) {
    screen.selection_begin(from.0, from.1);
    screen.selection_extend(to.0, to.1);
    screen.selection_end();
}

#[test]
fn selects_part_of_one_line() {
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "hello");

    // Columns are half-open, like every other range here.
    select(&mut screen, (0, 1), (0, 4));
    assert_eq!(screen.selection_text().as_deref(), Some("ell"));
}

#[test]
fn selects_across_several_lines() {
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "one\ntwo\nthree");

    select(&mut screen, (0, 1), (2, 3));
    assert_eq!(screen.selection_text().as_deref(), Some("ne\ntwo\nthr"));
}

#[test]
fn a_backwards_drag_selects_the_same_text() {
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "one\ntwo");

    select(&mut screen, (1, 3), (0, 1));
    assert_eq!(screen.selection_text().as_deref(), Some("ne\ntwo"));
}

#[test]
fn trailing_blanks_are_not_copied() {
    // A terminal pads every line to the full width. Pasting that padding back
    // is never what anyone wanted.
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "hi\nthere");

    select(&mut screen, (0, 0), (1, 10));
    assert_eq!(screen.selection_text().as_deref(), Some("hi\nthere"));
}

#[test]
fn an_empty_selection_copies_nothing() {
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "hello");

    select(&mut screen, (0, 2), (0, 2));
    assert!(!screen.has_selection());
    assert_eq!(screen.selection_text(), None);
}

#[test]
fn scrolling_the_view_leaves_the_selection_on_its_text() {
    // The whole reason selections are anchored to line ids: after scrolling,
    // the same text sits at a different screen row.
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "one\ntwo\nthree\nfour\nfive\nsix");

    // "one" has scrolled into the history; bring it back into view.
    screen.scroll_view(3);
    assert_eq!(
        screen.visible_row(0).iter().map(|c| c.ch).collect::<String>().trim_end(),
        "one"
    );

    select(&mut screen, (0, 0), (0, 3));
    assert_eq!(screen.selection_text().as_deref(), Some("one"));

    // Back to the bottom: the selection must still name "one", not whatever
    // is now on row 0.
    screen.scroll_view_to_bottom();
    assert_eq!(screen.selection_text().as_deref(), Some("one"));
}

#[test]
fn output_arriving_does_not_drag_the_selection_along() {
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "keep me\n");

    screen.scroll_view(0);
    select(&mut screen, (0, 0), (0, 7));
    assert_eq!(screen.selection_text().as_deref(), Some("keep me"));

    // Enough output to push the selected line up and off the screen.
    write(&mut screen, "a\nb\nc\nd\ne\n");
    assert_eq!(screen.selection_text().as_deref(), Some("keep me"));
}

#[test]
fn a_selection_survives_the_scrollback_overflowing() {
    // Line ids are offset by the number of evicted lines precisely so that
    // this keeps working; a raw index into the ring would silently slide onto
    // different text.
    let mut screen = screen_with_scrollback(4);
    write(&mut screen, "target\n");
    select(&mut screen, (0, 0), (0, 6));
    assert_eq!(screen.selection_text().as_deref(), Some("target"));

    // Far more than the scrollback holds, so "target" is evicted entirely.
    for i in 0..20 {
        write(&mut screen, &format!("line{i}\n"));
    }

    // The text is gone, so there is nothing to copy -- but it must not have
    // quietly become some *other* line's text.
    let copied = screen.selection_text();
    assert!(
        copied.is_none() || copied.as_deref() == Some("target"),
        "selection drifted onto {copied:?}",
    );
}

#[test]
fn resizing_drops_the_selection() {
    // Reflowing moves every line; keeping the selection would point it at
    // text that is no longer there.
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "hello");
    select(&mut screen, (0, 0), (0, 5));
    assert!(screen.has_selection());

    screen.resize(Size { width: 20, height: 6 });
    assert!(!screen.has_selection());
}

#[test]
fn switching_to_the_alternate_screen_drops_the_selection() {
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "hello");
    select(&mut screen, (0, 0), (0, 5));

    screen.set_alternate(true);
    assert!(!screen.has_selection());
}

#[test]
fn is_selected_marks_exactly_the_selected_cells() {
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "abcdef");

    select(&mut screen, (0, 2), (0, 4));

    assert!(!screen.is_selected(0, 1));
    assert!(screen.is_selected(0, 2));
    assert!(screen.is_selected(0, 3));
    // Exclusive at the far end, matching the copied text "cd".
    assert!(!screen.is_selected(0, 4));
    assert_eq!(screen.selection_text().as_deref(), Some("cd"));
}

#[test]
fn a_drag_in_progress_is_reported_until_it_ends() {
    // The UI uses this to tell "start a new selection" from "extend the one
    // being dragged", so it has to be exact.
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "hello");

    assert!(!screen.is_selecting());
    screen.selection_begin(0, 0);
    assert!(screen.is_selecting());
    screen.selection_extend(0, 3);
    assert!(screen.is_selecting());
    screen.selection_end();
    assert!(!screen.is_selecting());

    // Ending the drag keeps what was selected.
    assert_eq!(screen.selection_text().as_deref(), Some("hel"));
}

#[test]
fn clearing_reports_whether_there_was_anything_to_clear() {
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "hello");

    assert!(!screen.selection_clear(), "nothing selected yet");
    select(&mut screen, (0, 0), (0, 3));
    assert!(screen.selection_clear());
    assert!(!screen.has_selection());
}

#[test]
fn wide_characters_are_copied_once() {
    // Each occupies two cells, the second of which carries no text.
    let mut screen = screen_with_scrollback(100);
    write(&mut screen, "漢字");

    select(&mut screen, (0, 0), (0, 4));
    assert_eq!(screen.selection_text().as_deref(), Some("漢字"));
}
