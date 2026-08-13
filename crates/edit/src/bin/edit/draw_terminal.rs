// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The terminal panel below the editor.
//!
//! The panel holds any number of terminals as tabs. Only the active one is
//! drawn, but every one of them is polled each frame: a hidden terminal's
//! reader thread keeps appending to its buffer, and a `cargo build` left
//! running in a background tab would otherwise grow it without bound.

use std::rc::Rc;

use edit::cell::SemiRefCell;
use edit::helpers::*;
use edit::input::{kbmod, vk};
use edit::terminal::{RcTerminal, Terminal};
use edit::tui::*;
use stdext::arena_format;
use stdext::collections::BString;

use crate::localization::*;
use crate::state::*;

/// Height of the panel's tab bar.
const TITLE_HEIGHT: CoordType = 1;

/// The panel's total height, including its tab bar. Zero when hidden.
pub fn terminal_panel_height(state: &State) -> CoordType {
    if state.terminal_visible { state.terminal_height + TITLE_HEIGHT } else { 0 }
}

pub fn draw_terminal(ctx: &mut Context, state: &mut State) {
    if !state.terminal_visible {
        return;
    }

    // The first shell only starts once the user actually asks for the panel,
    // so that not opening it costs nothing. Additional tabs are requested by
    // the shortcut handler, which runs before this and has no `ctx` sizing
    // information or a good place to report a failure.
    if state.terminal_wants_new || state.terminals.is_empty() {
        state.terminal_wants_new = false;
        if !spawn_terminal(ctx, state) && state.terminals.is_empty() {
            return;
        }
    }

    // Keep background tabs drained. `ctx.terminal()` polls the active one too,
    // but that second call is a no-op.
    let mut changed = false;
    for terminal in &state.terminals {
        changed |= terminal.borrow_mut().poll();
    }
    if changed {
        ctx.needs_rerender();
    }

    state.terminal_active = state.terminal_active.min(state.terminals.len() - 1);
    let active = state.terminals[state.terminal_active].clone();

    ctx.block_begin("terminal-panel");
    // Deliberately not a focus well: F6 has to be able to hand the focus back
    // to the editor, and a well would trap it in here.
    ctx.attr_intrinsic_size(Size {
        width: COORD_TYPE_SAFE_MAX,
        height: terminal_panel_height(state),
    });
    {
        draw_tab_bar(ctx, state);

        // The classname stays the same across tabs on purpose: reusing the
        // layout node means a newly shown terminal already knows its size,
        // instead of rendering once at the wrong size and resizing after.
        if ctx.terminal("terminal", active.clone()) {
            ctx.needs_rerender();
        }
        ctx.attr_intrinsic_size(Size { width: COORD_TYPE_SAFE_MAX, height: state.terminal_height });

        match state.terminal_wants_focus.take() {
            Some(true) => ctx.steal_focus(),
            Some(false) => ctx.toss_focus_up(),
            None => {}
        }

        // Remembered for the F6 handler, which runs before this and so can't
        // ask the tui directly.
        state.terminal_focused = ctx.is_focused();

        // Once a child is gone, Enter closes that tab rather than it vanishing
        // on its own -- you want to read the last error it printed.
        if active.borrow().exit_code().is_some()
            && ctx.is_focused()
            && ctx.consume_shortcut(vk::RETURN)
        {
            close_active_terminal(state);
            ctx.needs_rerender();
        }
    }
    ctx.block_end();
}

/// Spawns a shell and makes it the active tab. Returns false if that failed.
fn spawn_terminal(ctx: &mut Context, state: &mut State) -> bool {
    let size = Size { width: ctx.size().width, height: state.terminal_height };
    let cwd = state.file_picker_pending_dir.as_path();
    let cwd = if cwd.as_os_str().is_empty() { None } else { Some(cwd) };

    match Terminal::spawn_shell(cwd, size) {
        Ok(terminal) => {
            state.terminals.push(Rc::new(SemiRefCell::new(terminal)));
            state.terminal_active = state.terminals.len() - 1;
            state.terminal_wants_focus = Some(true);
            true
        }
        Err(err) => {
            error_log_add(ctx, state, err.into());
            if state.terminals.is_empty() {
                state.terminal_visible = false;
            }
            false
        }
    }
}

fn draw_tab_bar(ctx: &mut Context, state: &mut State) {
    ctx.table_begin("terminal-tabs");
    ctx.attr_background_rgba(state.menubar_color_bg);
    ctx.attr_foreground_rgba(state.menubar_color_fg);
    ctx.attr_intrinsic_size(Size { width: COORD_TYPE_SAFE_MAX, height: TITLE_HEIGHT });
    ctx.attr_padding(Rect::two(0, 1));
    ctx.table_set_cell_gap(Size { width: 1, height: 0 });
    {
        ctx.table_next_row();

        for (index, terminal) in state.terminals.iter().enumerate() {
            let label = tab_label(ctx, index, terminal);

            ctx.next_block_id_mixin(index as u64);
            ctx.label("tab", &label);
            ctx.attr_overflow(Overflow::TruncateTail);
            if index == state.terminal_active {
                ctx.attr_reverse();
            }
        }
    }
    ctx.table_end();
}

/// `2: ✳ Claude Code`, or `1: cmd.exe - exited with 0` once it's finished.
fn tab_label<'a>(ctx: &Context<'a, '_>, index: usize, terminal: &RcTerminal) -> BString<'a> {
    let term = terminal.borrow();

    let title = &term.screen().title;
    let title = if title.is_empty() { term.command() } else { title.as_str() };

    // A pseudo console reports the client's full image path as the title,
    // which would take up the entire tab bar. Titles an application set for
    // itself ("✳ Claude Code") contain spaces and are left alone.
    let title = match title.contains(' ') {
        false => title.rsplit(['\\', '/']).next().unwrap_or(title),
        true => title,
    };

    match term.exit_code() {
        Some(code) => {
            let suffix = loc(LocId::TerminalExited);
            let code = arena_format!(ctx.arena(), "{code}");
            arena_format!(
                ctx.arena(),
                " {}: {title} - {} ",
                index + 1,
                suffix.replace("{code}", &code)
            )
        }
        None => arena_format!(ctx.arena(), " {}: {title} ", index + 1),
    }
}

/// Handles the shortcuts that work no matter where the focus is.
pub fn draw_terminal_shortcuts(ctx: &mut Context, state: &mut State) {
    let Some(key) = ctx.keyboard_input() else {
        return;
    };

    if key == vk::F12 {
        toggle_terminal(state);
    } else if key == kbmod::SHIFT | vk::F12 {
        new_terminal(state);
    } else if key == kbmod::CTRL | vk::F12 {
        next_terminal(state);
    } else if key == vk::F6 && state.terminal_visible {
        // The one key that always gets you back out of the panel.
        state.terminal_wants_focus = Some(!state.terminal_focused);
    } else if key == kbmod::CTRL_SHIFT | vk::UP {
        resize_terminal(ctx, state, 1);
    } else if key == kbmod::CTRL_SHIFT | vk::DOWN {
        resize_terminal(ctx, state, -1);
    } else {
        return;
    }

    ctx.needs_rerender();
    ctx.set_input_consumed();
}

pub fn toggle_terminal(state: &mut State) {
    state.terminal_visible = !state.terminal_visible;
    state.terminal_wants_focus = Some(state.terminal_visible);
}

/// Opens another tab. The panel takes care of the actual spawning, so that
/// failures are reported the same way wherever they come from.
pub fn new_terminal(state: &mut State) {
    state.terminal_visible = true;
    state.terminal_wants_new = true;
    state.terminal_wants_focus = Some(true);
}

pub fn next_terminal(state: &mut State) {
    if state.terminals.len() < 2 {
        return;
    }
    state.terminal_active = (state.terminal_active + 1) % state.terminals.len();
    state.terminal_wants_focus = Some(true);
}

/// Closes the active tab, hiding the panel when it was the last one.
pub fn close_active_terminal(state: &mut State) {
    if state.terminals.is_empty() {
        return;
    }

    state.terminals.remove(state.terminal_active).borrow_mut().close();

    if state.terminals.is_empty() {
        state.terminal_visible = false;
        state.terminal_wants_focus = Some(false);
    } else {
        state.terminal_active = state.terminal_active.min(state.terminals.len() - 1);
        state.terminal_wants_focus = Some(true);
    }
}

/// Replaces the active tab with a fresh shell.
pub fn restart_terminal(state: &mut State) {
    close_active_terminal(state);
    new_terminal(state);
}

fn resize_terminal(ctx: &Context, state: &mut State, delta: CoordType) {
    if !state.terminal_visible {
        return;
    }
    // Leave room for the menu bar, status bar, and at least one editor line.
    let max = (ctx.size().height - 4).max(TERMINAL_MIN_HEIGHT);
    state.terminal_height = (state.terminal_height + delta).clamp(TERMINAL_MIN_HEIGHT, max);
}

/// Makes sure no child process outlives the editor.
pub fn shutdown_terminal(state: &mut State) {
    for terminal in state.terminals.drain(..) {
        terminal.borrow_mut().close();
    }
}
