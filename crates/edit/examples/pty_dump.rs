// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Runs a command under a pseudo console, feeds its output through the
//! terminal emulator, and prints the resulting screen.
//!
//! This exists to check the emulator against real applications without having
//! to wire up any UI. Anything the emulator didn't understand is listed at the
//! end, which is the quickest way to find out what a given TUI needs.
//!
//! ```text
//! cargo run --example pty_dump -- 3 cmd.exe
//! cargo run --example pty_dump -- 8 --keys $'hello\r' claude
//! ```

use std::time::{Duration, Instant};
use std::{env, process, thread};

use edit::helpers::*;
use edit::sys;
use edit::terminal::emulator::Emulator;
use edit::terminal::screen::{CellAttributes, Screen};

fn main() -> process::ExitCode {
    let mut args = env::args().skip(1);

    let Some(seconds) = args.next().and_then(|s| s.parse::<u64>().ok()) else {
        eprintln!("usage: pty_dump <seconds> [--keys <input>] <command>...");
        return process::ExitCode::FAILURE;
    };

    // Each `--keys` is sent as its own batch, a second apart. Sending
    // everything at once isn't representative: the editor only settles focus
    // changes between batches, so a burst behaves differently to a person.
    let mut keys: Vec<String> = Vec::new();
    let mut command = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--keys" && command.is_empty() {
            keys.push(
                args.next()
                    .unwrap_or_default()
                    .replace("\\r", "\r")
                    .replace("\\n", "\n")
                    .replace("\\e", "\x1b"),
            );
        } else {
            command.push(arg);
        }
    }

    if command.is_empty() {
        eprintln!("usage: pty_dump <seconds> [--keys <input>] <command>...");
        return process::ExitCode::FAILURE;
    }

    let size = Size { width: 100, height: 30 };
    let command = command.join(" ");

    let (pty, mut reader) = match sys::Pty::spawn(&command, None, size) {
        Ok(pty) => pty,
        Err(err) => {
            eprintln!("failed to spawn {command:?}: {err}");
            return process::ExitCode::FAILURE;
        }
    };

    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0; 16 * KIBI];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut screen = Screen::new(size, 1000);
    let mut emulator = Emulator::new();
    let mut reply = Vec::new();
    // A pty read can end in the middle of a UTF-8 sequence.
    let mut pending = Vec::new();
    let mut total = 0usize;

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut keys = keys.into_iter();
    let mut next_keys_at = Instant::now() + Duration::from_millis(1500);

    while Instant::now() < deadline {
        if Instant::now() >= next_keys_at
            && let Some(batch) = keys.next()
        {
            let _ = pty.write(batch.as_bytes());
            next_keys_at = Instant::now() + Duration::from_millis(1000);
        }

        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
            total += chunk.len();
            pending.extend_from_slice(&chunk);

            // Feed only the part that is complete UTF-8 and keep the rest.
            let valid = match std::str::from_utf8(&pending) {
                Ok(_) => pending.len(),
                Err(err) => err.valid_up_to(),
            };
            let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
            pending.drain(..valid);

            emulator.consume(&mut screen, &text, &mut reply);

            if !reply.is_empty() {
                let _ = pty.write(&reply);
                reply.clear();
            }
        }
    }

    print_screen(&screen, size);

    println!("\n--- {total} bytes, title {:?} ---", screen.title);
    println!(
        "alternate screen: {}, cursor visible: {}, mouse: {:?}, bracketed paste: {}",
        screen.on_alternate(),
        screen.cursor_visible,
        screen.mouse_mode,
        screen.bracketed_paste,
    );

    // Handy for checking that a theme change really reached the screen:
    // the grid text is identical either way, only the colors differ.
    if env::var_os("PTY_DUMP_COLORS").is_some() {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for y in 0..size.height {
            for cell in screen.visible_row(y) {
                if cell.ch == ' ' {
                    continue;
                }
                let key = format!("{:?}", cell.fg);
                match seen.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, count)) => *count += 1,
                    None => seen.push((key, 1)),
                }
            }
        }
        seen.sort();
        println!("foreground colors in use:");
        for (color, count) in seen {
            println!("  {color} x{count}");
        }
    }

    if emulator.unknown.is_empty() {
        println!("unhandled sequences: none");
    } else {
        println!("unhandled sequences:");
        for entry in emulator.unknown.iter() {
            println!("  {entry}");
        }
    }

    drop(pty);
    process::ExitCode::SUCCESS
}

fn print_screen(screen: &Screen, size: Size) {
    let border: String = "-".repeat(size.width as usize);
    println!("+{border}+");

    for y in 0..size.height {
        let mut line = String::new();
        for cell in screen.visible_row(y) {
            if cell.attr.has(CellAttributes::WIDE_TRAILER) {
                continue;
            }
            line.push(cell.ch);
        }
        println!("|{}|", line.trim_end());
    }

    println!("+{border}+");
}
