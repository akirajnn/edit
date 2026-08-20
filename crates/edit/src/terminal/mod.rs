// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! An embedded terminal.
//!
//! [`screen`] holds what the child process drew, [`emulator`] translates the
//! child's VT output into changes to it, [`keymap`] turns editor input back
//! into the bytes a terminal would send. [`Terminal`] ties those to a pty.

pub mod emulator;
pub mod keymap;
pub mod screen;
#[cfg(test)]
mod selection_tests;

use std::io;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;

use self::emulator::Emulator;
use self::screen::{DEFAULT_SCROLLBACK, Screen};
use crate::cell::SemiRefCell;
use crate::helpers::*;
use crate::input::InputMouseState;
use crate::sys;

/// A [`Terminal`] with inner mutability, matching how the TUI borrows buffers.
pub type TerminalCell = SemiRefCell<Terminal>;

/// A [`Terminal`] inside an [`Rc`].
pub type RcTerminal = Rc<TerminalCell>;

/// What the reader thread hands over to the main thread.
#[derive(Default)]
struct PendingOutput {
    bytes: Vec<u8>,
    /// Set once the pseudo console is gone and no more output can arrive.
    eof: bool,
}

/// A child process, its pseudo console, and the screen it draws on.
pub struct Terminal {
    /// `None` once the child exited and we tore the pty down.
    pty: Option<sys::Pty>,
    pending: Arc<Mutex<PendingOutput>>,

    screen: Screen,
    emulator: Emulator,

    /// A read can end mid-character, so the tail waits here for the next one.
    partial_utf8: Vec<u8>,
    /// Answers to the child's questions, written back on the next poll.
    reply: Vec<u8>,

    exit_code: Option<u32>,
    command: String,

    /// The last mouse state we reported, so that a button being held down
    /// doesn't turn into one event per frame.
    last_mouse: (InputMouseState, Point),
}

impl Terminal {
    /// Spawns `command` in a pseudo console of the given size.
    pub fn spawn(
        command: &str,
        cwd: Option<&Path>,
        size: Size,
        scrollback: usize,
    ) -> io::Result<Self> {
        let (pty, mut reader) = sys::Pty::spawn(command, cwd, size)?;
        let pending = Arc::new(Mutex::new(PendingOutput::default()));

        // The reader blocks until the pty is dropped, so it's deliberately not
        // joined anywhere; see the `sys::windows_pty` module docs.
        let sink = pending.clone();
        thread::spawn(move || {
            let mut buf = [0; 32 * KIBI];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let Ok(mut pending) = sink.lock() else {
                            break;
                        };
                        pending.bytes.extend_from_slice(&buf[..n]);
                    }
                }
                // Nudge the main loop, which is otherwise blocked on stdin.
                sys::wake();
            }

            if let Ok(mut pending) = sink.lock() {
                pending.eof = true;
            }
            sys::wake();
        });

        Ok(Self {
            pty: Some(pty),
            pending,
            screen: Screen::new(size, scrollback),
            emulator: Emulator::new(),
            partial_utf8: Vec::new(),
            reply: Vec::new(),
            exit_code: None,
            command: command.to_string(),
            last_mouse: (InputMouseState::None, Point::MIN),
        })
    }

    /// Returns true if the mouse changed since the last call, and remembers it.
    pub fn take_mouse_change(&mut self, state: InputMouseState, position: Point) -> bool {
        let next = (state, position);
        let changed = next != self.last_mouse;
        self.last_mouse = next;
        changed
    }

    /// Spawns the user's shell.
    pub fn spawn_shell(cwd: Option<&Path>, size: Size) -> io::Result<Self> {
        Self::spawn(&sys::default_shell(), cwd, size, DEFAULT_SCROLLBACK)
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit_code
    }

    pub fn is_running(&self) -> bool {
        self.pty.is_some() && self.exit_code.is_none()
    }

    /// Sequences the emulator couldn't interpret, for the error log.
    pub fn unknown_sequences(&self) -> impl Iterator<Item = &str> {
        self.emulator.unknown.iter()
    }

    /// Applies everything the child produced since the last call.
    ///
    /// Returns `true` if the screen changed and the UI has to redraw.
    pub fn poll(&mut self) -> bool {
        let (bytes, eof) = {
            let Ok(mut pending) = self.pending.lock() else {
                return false;
            };
            (std::mem::take(&mut pending.bytes), pending.eof)
        };

        let generation = self.screen.generation();

        if !bytes.is_empty() {
            self.partial_utf8.extend_from_slice(&bytes);

            // Feed the emulator the longest complete prefix and keep the rest.
            // Splitting a character in half would put a replacement glyph on
            // the screen that never goes away.
            let valid = match std::str::from_utf8(&self.partial_utf8) {
                Ok(_) => self.partial_utf8.len(),
                Err(err) => err.valid_up_to(),
            };

            if valid > 0 {
                let text = String::from_utf8_lossy(&self.partial_utf8[..valid]).into_owned();
                self.partial_utf8.drain(..valid);
                self.emulator.consume(&mut self.screen, &text, &mut self.reply);
            } else if self.partial_utf8.len() > 4 {
                // Not valid UTF-8 and too long to be a truncated character:
                // drop a byte so a broken stream can't stall us forever.
                self.partial_utf8.drain(..1);
            }

            if !self.reply.is_empty() {
                if let Some(pty) = &self.pty {
                    let _ = pty.write(&self.reply);
                }
                self.reply.clear();
            }
        }

        // The output pipe only reports EOF once *we* close the pseudo console,
        // so a finished child has to be noticed by asking for its exit code.
        // We keep the pty around afterwards, which means anything the pseudo
        // console renders after the fact still lands on the screen.
        if self.exit_code.is_none()
            && let Some(pty) = &self.pty
        {
            if let Some(code) = pty.try_exit_code() {
                self.exit_code = Some(code);
            } else if eof {
                // The pipe broke without us asking. Nothing more is coming.
                self.exit_code = Some(0);
            }
        }

        self.screen.generation() != generation
    }

    /// Sends raw bytes to the child.
    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Typing scrolls back to the bottom, like every other terminal.
        self.screen.scroll_view_to_bottom();
        if let Some(pty) = &self.pty {
            let _ = pty.write(bytes);
        }
    }

    /// Tells both our screen and the child about a new size.
    pub fn resize(&mut self, size: Size) {
        if size == self.screen.size() {
            return;
        }
        self.screen.resize(size);
        if let Some(pty) = &self.pty {
            let _ = pty.resize(size);
        }
    }

    /// Kills the child and releases the pseudo console.
    pub fn close(&mut self) {
        if let Some(pty) = self.pty.take() {
            if self.exit_code.is_none() {
                self.exit_code = pty.try_exit_code().or(Some(0));
            }
            drop(pty);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod test_helpers {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    pub const SIZE: Size = Size { width: 60, height: 12 };

    /// Polls until `predicate` holds, so tests don't depend on the pty's timing.
    pub fn poll_until(term: &mut Terminal, what: &str, predicate: impl Fn(&Terminal) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            term.poll();
            if predicate(term) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}:\n{}", dump(term));
    }

    pub fn dump(term: &Terminal) -> String {
        let mut out = String::new();
        for y in 0..term.screen().size().height {
            for cell in term.screen().visible_row(y) {
                // The filler cell of a wide character isn't part of the text.
                if !cell.attr.has(screen::CellAttributes::WIDE_TRAILER) {
                    out.push(cell.ch);
                }
            }
            out.push('\n');
        }
        out
    }

    pub fn contains(term: &Terminal, needle: &str) -> bool {
        dump(term).contains(needle)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::test_helpers::*;
    use super::*;

    #[test]
    fn runs_a_command_and_shows_its_output() {
        let mut term = Terminal::spawn("cmd.exe", None, SIZE, 100).unwrap();
        term.write(b"echo terminal-works\r\n");
        poll_until(&mut term, "the echoed output", |t| contains(t, "terminal-works"));
    }

    #[test]
    fn notices_the_child_exiting() {
        let mut term = Terminal::spawn("cmd.exe /c exit 3", None, SIZE, 100).unwrap();
        poll_until(&mut term, "the exit code", |t| t.exit_code().is_some());
        assert_eq!(term.exit_code(), Some(3));
        assert!(!term.is_running());
    }

    #[test]
    fn resize_reaches_the_child() {
        let mut term = Terminal::spawn("cmd.exe", None, SIZE, 100).unwrap();
        term.resize(Size { width: 100, height: 30 });
        assert_eq!(term.screen().size(), Size { width: 100, height: 30 });

        // `%COLUMNS%` isn't a thing in cmd, so ask the console itself.
        term.write(b"mode con\r\n");
        poll_until(&mut term, "the reported width", |t| contains(t, "100"));
    }

    #[test]
    fn survives_output_split_mid_character() {
        // The pty hands over arbitrary byte counts, and CJK output is the
        // usual way a multi-byte character ends up straddling two reads.
        let mut term = Terminal::spawn("cmd.exe", None, SIZE, 100).unwrap();
        term.write("echo 漢字測試\r\n".as_bytes());
        poll_until(&mut term, "the wide characters", |t| contains(t, "漢字測試"));
        assert!(!dump(&term).contains('\u{fffd}'), "replacement characters in:\n{}", dump(&term));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::test_helpers::*;
    use super::*;

    #[test]
    fn runs_a_command_and_shows_its_output() {
        let mut term = Terminal::spawn("sh", None, SIZE, 100).unwrap();
        term.write(b"echo terminal-works\n");
        poll_until(&mut term, "the echoed output", |t| contains(t, "terminal-works"));
    }

    #[test]
    fn notices_the_child_exiting() {
        // `parse_command` only splits on whitespace, so `sh -c 'exit 3'`
        // wouldn't reach the shell intact; drive it interactively instead.
        let mut term = Terminal::spawn("sh", None, SIZE, 100).unwrap();
        term.write(b"exit 3\n");
        poll_until(&mut term, "the exit code", |t| t.exit_code().is_some());
        assert_eq!(term.exit_code(), Some(3));
        assert!(!term.is_running());
    }

    #[test]
    fn resize_reaches_the_child() {
        let mut term = Terminal::spawn("sh", None, SIZE, 100).unwrap();
        term.resize(Size { width: 100, height: 30 });
        assert_eq!(term.screen().size(), Size { width: 100, height: 30 });

        // Ask the child's tty itself, rather than trusting our own screen size.
        term.write(b"stty size\n");
        poll_until(&mut term, "the reported size", |t| contains(t, "30 100"));
    }

    #[test]
    fn survives_output_split_mid_character() {
        // The pty hands over arbitrary byte counts, and CJK output is the
        // usual way a multi-byte character ends up straddling two reads.
        let mut term = Terminal::spawn("sh", None, SIZE, 100).unwrap();
        term.write("echo 漢字測試\n".as_bytes());
        poll_until(&mut term, "the wide characters", |t| contains(t, "漢字測試"));
        assert!(!dump(&term).contains('\u{fffd}'), "replacement characters in:\n{}", dump(&term));
    }
}
