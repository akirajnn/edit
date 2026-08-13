// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pseudo terminal support. Not implemented yet on Unix.
//!
//! The shape of this API mirrors `windows_pty`, so filling it in later is a
//! matter of writing the `posix_openpt`/`grantpt`/`unlockpt` dance plus a fork
//! that calls `setsid` and `TIOCSCTTY`. No caller has to change.

use std::io;
use std::path::Path;

use crate::helpers::*;

pub struct Pty {
    _private: (),
}

pub struct PtyReader {
    _private: (),
}

impl Pty {
    pub fn spawn(_command: &str, _cwd: Option<&Path>, _size: Size) -> io::Result<(Pty, PtyReader)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the terminal panel is only implemented on Windows so far",
        ))
    }

    pub fn write(&self, _data: &[u8]) -> io::Result<()> {
        unreachable!()
    }

    pub fn resize(&self, _size: Size) -> io::Result<()> {
        unreachable!()
    }

    pub fn try_exit_code(&self) -> Option<u32> {
        unreachable!()
    }
}

impl PtyReader {
    pub fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        unreachable!()
    }
}

pub fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Interrupts a blocking `read_stdin` so that the caller redraws.
///
/// A no-op until the Unix side exists: with no pty there's no background thread
/// that could ask for a redraw. Implementing it means adding a self-pipe to the
/// `poll` set in `unix::read_stdin`.
pub fn wake() {}
