// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reacting to files being modified outside the editor.
//!
//! [`crate::watcher`] notices that *something* changed; this decides what to do
//! about it. A document with no unsaved changes is simply reloaded, because
//! there is nothing to lose and silently showing stale content is worse. A
//! document with unsaved changes is never touched without asking.

use edit::framebuffer::IndexedColor;
use edit::helpers::*;
use edit::input::vk;
use edit::lsh::HighlightKind;
use edit::oklab::StraightRgba;
use edit::theme;
use edit::tui::*;
use stdext::arena_format;
use stdext::collections::BString;

use crate::diff::{self, DiffLine, DiffResult};
use crate::documents::Document;
use crate::localization::*;
use crate::state::*;

/// Reloads what can be reloaded, and flags the rest for [`draw_reload_prompt`].
///
/// Called from the main loop when the watcher reports a change.
pub fn handle_external_changes(state: &mut State) {
    let mut conflict = None;
    let mut errors = Vec::new();

    for doc in state.documents.iter_mut() {
        if !doc.changed_on_disk() {
            continue;
        }

        if doc.buffer.borrow().is_dirty() {
            // Don't ask twice for the same file while a dialog is already up.
            if conflict.is_none() {
                // Computed once, here, rather than on every frame the dialog
                // is up: it reads the file off disk and walks both versions.
                conflict = Some(ReloadPrompt {
                    diff: describe_change(doc),
                    filename: doc.filename.clone(),
                });
            }
            continue;
        }

        match doc.reload_preserving_cursor() {
            Ok(()) => {}
            Err(err) => {
                // Accept the new state anyway; retrying every second would
                // just produce the same error over and over.
                doc.accept_disk_state();
                errors.push(err);
            }
        }
    }

    for err in errors {
        state.add_error(err);
    }

    if conflict.is_some() && state.wants_reload_prompt.is_none() {
        state.wants_reload_prompt = conflict;
    }
}

/// Diffs what the editor holds against what's on disk.
///
/// Failing to read the file isn't worth reporting as an error of its own: the
/// dialog still works, it just can't show what changed.
fn describe_change(doc: &Document) -> DiffResult {
    let Some(path) = &doc.path else {
        return DiffResult::TooLarge;
    };

    let Ok(theirs) = std::fs::read(path) else {
        return DiffResult::TooLarge;
    };

    let mine = {
        let tb = doc.buffer.borrow();
        let document = tb.as_document();
        let mut text = Vec::with_capacity(tb.text_length());
        let mut offset = 0;
        loop {
            let chunk = document.read_forward(offset);
            if chunk.is_empty() {
                break;
            }
            text.extend_from_slice(chunk);
            offset += chunk.len();
        }
        text
    };

    // A preview is for reading, so a file that isn't valid UTF-8 is shown
    // with replacement characters rather than refused.
    diff::diff_lines(&String::from_utf8_lossy(&mine), &String::from_utf8_lossy(&theirs))
}

pub fn draw_reload_prompt(ctx: &mut Context, state: &mut State) {
    let Some(prompt) = state.wants_reload_prompt.take() else {
        return;
    };
    let filename = prompt.filename.clone();

    enum Action {
        None,
        Reload,
        KeepMine,
    }
    let mut action = Action::None;

    ctx.modal_begin("file-changed", loc(LocId::FileChangedTitle));
    {
        let contains_focus = ctx.contains_focus();

        ctx.block_begin("description");
        ctx.attr_padding(Rect::three(1, 2, 0));
        {
            let line = {
                let template = loc(LocId::FileChangedDescription);
                let mut text = BString::empty();
                text.push_str(ctx.arena(), template);
                text.replace_once_in_place(ctx.arena(), "{file}", &filename);
                text
            };
            ctx.label("line1", &line);
            ctx.attr_position(Position::Center);
            ctx.label("line2", loc(LocId::FileChangedUnsaved));
            ctx.attr_position(Position::Center);
            // Without this the two colors are ambiguous: in a conflict `-` is
            // not "the old version", it's *your* version.
            ctx.label("legend", loc(LocId::FileChangedLegend));
            ctx.attr_position(Position::Center);
        }
        ctx.block_end();

        draw_diff(ctx, &prompt.diff);

        ctx.table_begin("choices");
        ctx.inherit_focus();
        ctx.attr_padding(Rect::three(0, 2, 1));
        ctx.attr_position(Position::Center);
        ctx.table_set_cell_gap(Size { width: 2, height: 0 });
        {
            ctx.table_next_row();
            ctx.inherit_focus();

            if ctx.button(
                "reload",
                loc(LocId::FileChangedReload),
                ButtonStyle::default().accelerator('R'),
            ) {
                action = Action::Reload;
            }

            if ctx.button(
                "keep",
                loc(LocId::FileChangedKeepMine),
                ButtonStyle::default().accelerator('K'),
            ) {
                action = Action::KeepMine;
            }
            // Default to the non-destructive choice.
            ctx.inherit_focus();

            if contains_focus {
                if ctx.consume_shortcut(vk::R) {
                    action = Action::Reload;
                } else if ctx.consume_shortcut(vk::K) {
                    action = Action::KeepMine;
                }
            }
        }
        ctx.table_end();
    }
    if ctx.modal_end() {
        // Dismissing without choosing keeps the user's work.
        action = Action::KeepMine;
    }

    let discard = match action {
        // Taken out of `state` at the top so the diff could be borrowed while
        // the rest of `state` stays mutable. Nothing was decided, so put it
        // back or the dialog would close itself after one frame.
        Action::None => {
            state.wants_reload_prompt = Some(prompt);
            return;
        }
        Action::Reload => true,
        Action::KeepMine => false,
    };

    let mut errors = Vec::new();

    for doc in state.documents.iter_mut() {
        if doc.filename != filename || !doc.changed_on_disk() {
            continue;
        }

        if discard
            && let Err(err) = doc.reload_preserving_cursor()
        {
            errors.push(err);
        }
        // Either way the on-disk state is now acknowledged, so the same
        // change doesn't come back a second later.
        doc.accept_disk_state();
    }

    for err in errors {
        state.add_error(err);
    }

    ctx.needs_rerender();
}

/// How many diff lines the dialog will render before giving up.
const MAX_RENDERED: usize = 500;

/// Renders the diff, colored with the active theme's markup colors so it
/// matches how the editor shows a `.diff` file.
fn draw_diff(ctx: &mut Context, diff: &DiffResult) {
    let removed = theme_color(ctx, HighlightKind::MarkupDeleted, IndexedColor::Red);
    let added = theme_color(ctx, HighlightKind::MarkupInserted, IndexedColor::Green);
    let elided = ctx.indexed(IndexedColor::BrightBlack);

    let lines = match diff {
        DiffResult::Lines(lines) => lines,
        DiffResult::Identical => {
            // The stamp changed but the content didn't: a touch, or a write of
            // identical bytes. Worth saying, since "nothing changed" would
            // otherwise look like a bug.
            ctx.label("diff-identical", loc(LocId::FileChangedIdentical));
            ctx.attr_position(Position::Center);
            return;
        }
        DiffResult::Summary { removed, added } => {
            let text = arena_format!(
                ctx.arena(),
                "{}",
                loc(LocId::FileChangedSummary)
                    .replace("{removed}", &removed.to_string())
                    .replace("{added}", &added.to_string())
            );
            ctx.label("diff-summary", &text);
            ctx.attr_position(Position::Center);
            return;
        }
        DiffResult::TooLarge => {
            ctx.label("diff-too-large", loc(LocId::FileChangedTooLarge));
            ctx.attr_position(Position::Center);
            return;
        }
    };

    let width = (ctx.size().width - 12).max(20);
    let height = (ctx.size().height - 14).max(4);

    ctx.scrollarea_begin("diff", Size { width, height });
    ctx.attr_padding(Rect::two(0, 1));
    {
        for (index, line) in lines.iter().take(MAX_RENDERED).enumerate() {
            ctx.next_block_id_mixin(index as u64);
            ctx.styled_label_begin("line");

            match line {
                DiffLine::Context(text) => {
                    ctx.styled_label_add_text(" ");
                    ctx.styled_label_add_text(text);
                }
                DiffLine::Removed(text) => {
                    ctx.styled_label_set_foreground(removed);
                    ctx.styled_label_add_text("-");
                    ctx.styled_label_add_text(text);
                }
                DiffLine::Added(text) => {
                    ctx.styled_label_set_foreground(added);
                    ctx.styled_label_add_text("+");
                    ctx.styled_label_add_text(text);
                }
                DiffLine::Skipped(count) => {
                    ctx.styled_label_set_foreground(elided);
                    let text = arena_format!(
                        ctx.arena(),
                        "{}",
                        loc(LocId::FileChangedSkipped).replace("{count}", &count.to_string())
                    );
                    ctx.styled_label_add_text(&text);
                }
            }

            ctx.styled_label_end();
            ctx.attr_overflow(Overflow::TruncateTail);
        }

        if lines.len() > MAX_RENDERED {
            ctx.styled_label_begin("truncated");
            ctx.styled_label_set_foreground(elided);
            ctx.styled_label_add_text(loc(LocId::FileChangedTruncated));
            ctx.styled_label_end();
        }
    }
    ctx.scrollarea_end();
}

/// The theme's color for a highlight kind, falling back when the theme leaves
/// that kind uncolored (`muted` does for several).
fn theme_color(ctx: &Context, kind: HighlightKind, fallback: IndexedColor) -> StraightRgba {
    ctx.indexed(theme::color_for(kind).unwrap_or(fallback))
}
