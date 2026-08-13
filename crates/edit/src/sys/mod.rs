// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Platform abstractions.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
mod unix_pty;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_pty;

#[cfg(not(windows))]
pub use std::fs::canonicalize;

#[cfg(unix)]
pub use unix::*;
#[cfg(unix)]
pub use unix_pty::*;
#[cfg(windows)]
pub use windows::*;
#[cfg(windows)]
pub use windows_pty::*;
