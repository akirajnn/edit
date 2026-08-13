// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Markdown preview, rendered by an external tool in a popup.
//!
//! The editor doesn't render Markdown itself. It runs whatever
//! `markdown.previewCommand` points at (`glow` by default) in a pseudo
//! console and shows the result, which is the same machinery the terminal
//! panel uses. The preview is therefore whatever that tool produces, and
//! swapping it for `mdcat` or anything else is a settings change.

use std::rc::Rc;

use edit::cell::SemiRefCell;
use edit::helpers::*;
use edit::terminal::Terminal;
use edit::tui::*;
use stdext::arena_format;

use crate::localization::*;
use crate::settings::Settings;
use crate::state::*;

/// Used when `markdown.previewCommand` isn't set.
///
/// Deliberately without glow's `-p`: its pager shells out to `less`, which
/// Windows doesn't have, so the popup would show glow's error instead of the
/// document. Scrolling is handled on our side once the command exits.
pub const DEFAULT_PREVIEW_COMMAND: &str = "glow";

/// Opens the preview, or closes it if it's already open.
pub fn toggle_markdown_preview(state: &mut State) {
    if state.preview.is_some() {
        close_markdown_preview(state);
    } else {
        state.wants_preview = true;
    }
}

pub fn close_markdown_preview(state: &mut State) {
    if let Some(terminal) = state.preview.take() {
        terminal.borrow_mut().close();
    }
    if let Some(path) = state.preview_temp.take() {
        let _ = std::fs::remove_file(path);
    }
    state.wants_preview = false;
}

pub fn draw_markdown_preview(ctx: &mut Context, state: &mut State) {
    if state.wants_preview && state.preview.is_none() {
        state.wants_preview = false;
        if !spawn_preview(ctx, state) {
            return;
        }
    }

    let Some(preview) = state.preview.clone() else {
        return;
    };

    preview.borrow_mut().poll();

    let width = (ctx.size().width - 8).max(20);
    let height = (ctx.size().height - 6).max(6);

    ctx.modal_begin("markdown-preview", loc(LocId::ViewMarkdownPreview));
    {
        if ctx.terminal("preview", preview.clone()) {
            ctx.needs_rerender();
        }
        ctx.attr_intrinsic_size(Size { width, height });

        if state.preview_wants_focus {
            state.preview_wants_focus = false;
            ctx.steal_focus();
        }
    }
    if ctx.modal_end() {
        close_markdown_preview(state);
        ctx.needs_rerender();
    }
}

/// Writes the buffer out and starts the preview command on it.
///
/// Returns false if nothing was started, in which case a dialog explaining
/// why is already queued.
fn spawn_preview(ctx: &mut Context, state: &mut State) -> bool {
    let Some(doc) = state.documents.active() else {
        return false;
    };

    // Preview what's on screen, not what was last saved. That also means an
    // unsaved or untitled buffer previews fine.
    let mut path = std::env::temp_dir();
    path.push(format!("edit-preview-{}.md", std::process::id()));

    let contents = {
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

    if let Err(err) = std::fs::write(&path, contents) {
        error_log_add(ctx, state, err.into());
        return false;
    }

    let command = Settings::borrow()
        .markdown_preview_command
        .clone()
        .unwrap_or_else(|| DEFAULT_PREVIEW_COMMAND.to_string());

    // The command line is `<command> <file>`, quoted so a temp directory with
    // spaces in it doesn't come apart.
    let command_line = format!("{command} \"{}\"", path.display());
    let size = Size { width: (ctx.size().width - 10).max(20), height: (ctx.size().height - 8).max(6) };

    match Terminal::spawn(&command_line, None, size, 5000) {
        Ok(terminal) => {
            state.preview = Some(Rc::new(SemiRefCell::new(terminal)));
            state.preview_temp = Some(path);
            state.preview_wants_focus = true;
            true
        }
        Err(err) => {
            let _ = std::fs::remove_file(&path);
            // A missing tool is the common case and deserves an explanation
            // rather than a bare OS error.
            if err.kind() == std::io::ErrorKind::NotFound {
                state.wants_preview_missing = true;
            } else {
                error_log_add(ctx, state, err.into());
            }
            false
        }
    }
}

/// Shown when the preview command isn't installed or isn't on PATH.
pub fn draw_preview_missing(ctx: &mut Context, state: &mut State) {
    let command = Settings::borrow()
        .markdown_preview_command
        .clone()
        .unwrap_or_else(|| DEFAULT_PREVIEW_COMMAND.to_string());
    // Just the program, without whatever flags follow it.
    let program = command.split_whitespace().next().unwrap_or(&command).to_string();

    ctx.modal_begin("preview-missing", loc(LocId::PreviewMissingTitle));
    {
        ctx.block_begin("content");
        ctx.attr_padding(Rect::three(1, 2, 1));
        {
            let line = arena_format!(
                ctx.arena(),
                "{}",
                loc(LocId::PreviewMissingDescription).replace("{command}", &program)
            );
            ctx.label("line1", &line);
            ctx.attr_position(Position::Center);

            ctx.label("line2", loc(LocId::PreviewMissingInstall));
            ctx.attr_position(Position::Center);

            ctx.label("command", "winget install charmbracelet.glow");
            ctx.attr_position(Position::Center);

            ctx.label("line3", loc(LocId::PreviewMissingPath));
            ctx.attr_position(Position::Center);
        }
        ctx.block_end();

        if ctx.button("ok", loc(LocId::Ok), ButtonStyle::default()) {
            state.wants_preview_missing = false;
        }
        ctx.attr_position(Position::Center);
        ctx.inherit_focus();
    }
    if ctx.modal_end() {
        state.wants_preview_missing = false;
    }
}
