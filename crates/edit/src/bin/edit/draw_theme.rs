// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The syntax color theme picker.

use edit::framebuffer::IndexedColor;
use edit::helpers::*;
use edit::theme::{self, Theme};
use edit::tui::*;

use crate::localization::*;
use crate::settings::Settings;
use crate::state::*;

pub fn draw_dialog_theme_change(ctx: &mut Context, state: &mut State) {
    // Remembered so that dismissing the dialog undoes the previews.
    if state.theme_before_preview.is_none() {
        state.theme_before_preview = Some(current_theme());
    }

    let mut done = false;
    let mut chosen = None;

    ctx.modal_begin("theme", loc(LocId::ViewTheme));
    {
        let width = (ctx.size().width - 20).max(10);
        let height = (ctx.size().height - 10).max(6);

        ctx.scrollarea_begin("scrollarea", Size { width, height });
        ctx.attr_background_rgba(ctx.indexed_alpha(IndexedColor::Black, 1, 4));
        ctx.inherit_focus();
        {
            ctx.list_begin("themes");
            ctx.inherit_focus();

            for name in theme::builtin_names() {
                match ctx.list_item(name == state.theme_name, name) {
                    // Preview as the selection moves, so picking one is a
                    // matter of looking at your own code rather than guessing.
                    ListSelection::Selected => {
                        if let Some(theme) = theme::builtin(name) {
                            theme::setup(*theme);
                            ctx.needs_rerender();
                        }
                    }
                    ListSelection::Activated => {
                        chosen = Some(name.to_string());
                        done = true;
                    }
                    ListSelection::Unchanged => {}
                }
            }

            // A theme defined in settings.json won't be in the built-in list,
            // but it still deserves a way back to it.
            let custom = &Settings::borrow().theme_name;
            if !custom.is_empty() && theme::builtin(custom).is_none() {
                let custom = custom.clone();
                if ctx.list_item(custom == state.theme_name, &custom) == ListSelection::Activated {
                    chosen = Some(custom);
                    done = true;
                }
            }

            ctx.list_end();
        }
        ctx.scrollarea_end();
    }
    let cancelled = ctx.modal_end();

    if cancelled {
        // Put back whatever was active before the previews started.
        if let Some(theme) = state.theme_before_preview.take() {
            theme::setup(theme);
        }
        state.wants_theme_picker = false;
        ctx.needs_rerender();
        return;
    }

    if !done {
        return;
    }

    if let Some(name) = chosen {
        apply_theme(state, &name);

        if let Err(err) = Settings::persist_theme(&name) {
            error_log_add(ctx, state, err);
        }
    }

    state.theme_before_preview = None;
    state.wants_theme_picker = false;
    ctx.needs_rerender();
}

/// Applies a theme by name, falling back to whatever settings.json defined.
fn apply_theme(state: &mut State, name: &str) {
    state.theme_name = name.to_string();

    if let Some(theme) = theme::builtin(name) {
        theme::setup(*theme);
        return;
    }

    // Not a built-in, so it has to be the custom one the settings resolved.
    if let Some(theme) = Settings::borrow().theme {
        theme::setup(theme);
    }
}

/// The theme currently in effect, for restoring after a cancelled preview.
fn current_theme() -> Theme {
    let settings = Settings::borrow();
    match theme::builtin(&settings.theme_name) {
        Some(theme) => *theme,
        None => settings.theme.unwrap_or_else(Theme::from_default),
    }
}
