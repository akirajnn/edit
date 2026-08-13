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
use edit::tui::*;
use stdext::collections::BString;

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
                conflict = Some(doc.filename.clone());
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

pub fn draw_reload_prompt(ctx: &mut Context, state: &mut State) {
    let Some(filename) = state.wants_reload_prompt.clone() else {
        return;
    };

    enum Action {
        None,
        Reload,
        KeepMine,
    }
    let mut action = Action::None;

    ctx.modal_begin("file-changed", loc(LocId::FileChangedTitle));
    ctx.attr_background_rgba(ctx.indexed(IndexedColor::Yellow));
    ctx.attr_foreground_rgba(ctx.indexed(IndexedColor::Black));
    {
        let contains_focus = ctx.contains_focus();

        ctx.block_begin("description");
        ctx.attr_padding(Rect::three(1, 2, 1));
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
        }
        ctx.block_end();

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
        Action::None => return,
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

    state.wants_reload_prompt = None;
    ctx.needs_rerender();
}
