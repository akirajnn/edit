// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pseudo terminal (PTY) support for Unix.
//!
//! Uses the POSIX PTY API: `posix_openpt` / `grantpt` / `unlockpt`, then
//! `fork` + `exec` with `setsid` / `TIOCSCTTY` to give the child its own
//! controlling terminal.
//!
//! # Handle ownership
//!
//! The master fd is duplicated before forking.  [`Pty`] keeps one copy for
//! writing and resizing; [`PtyReader`] keeps the other for the reader thread.
//! Closing either copy does not close the other, so the reader thread stays
//! alive until the slave side disappears (child exits → EIO on the master).
//!
//! [`Pty`] deliberately does **not** join the reader thread on drop.  Instead
//! it kills the child and closes its master copy; the reader then sees EIO and
//! exits on its own.

use std::cell::Cell;
use std::ffi::{CStr, CString, c_int};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use crate::helpers::*;

/// A child process attached to a pseudo terminal.
pub struct Pty {
    /// Master side of the PTY; used for writing to the child.
    master_fd: c_int,
    /// PID of the child process.
    child_pid: libc::pid_t,
    /// Cached exit status once the child has been reaped by `try_exit_code`.
    /// Avoids calling `waitpid` on an already-reaped PID.
    cached_exit: Cell<Option<u32>>,
}

/// The read end of a [`Pty`]'s output.
///
/// This is the only piece that gets moved onto the reader thread.
pub struct PtyReader {
    /// A `dup` of the master fd, used only for reading.
    master_fd: c_int,
}

// SAFETY: A raw file descriptor has no thread affinity.
// `PtyReader` holds exclusive ownership of its fd for reading.
unsafe impl Send for PtyReader {}

impl Pty {
    /// Spawns `command` in a new pseudo terminal of the given size.
    ///
    /// `command` is split on ASCII whitespace to build the argument vector.
    pub fn spawn(command: &str, cwd: Option<&Path>, size: Size) -> io::Result<(Pty, PtyReader)> {
        let width = size.width.clamp(1, u16::MAX as CoordType) as u16;
        let height = size.height.clamp(1, u16::MAX as CoordType) as u16;

        let argv = parse_command(command)?;
        let cwd_c = cwd
            .map(|p| CString::new(p.as_os_str().as_bytes()))
            .transpose()
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "cwd contains a null byte")
            })?;

        unsafe {
            // Open master PTY.  We set O_CLOEXEC afterwards via fcntl so that
            // it doesn't survive into the child past exec.
            let master = check_ret(libc::posix_openpt(libc::O_RDWR))?;
            set_cloexec(master);
            let master = OwnedFd(master);

            check_ret(libc::grantpt(master.0))?;
            check_ret(libc::unlockpt(master.0))?;

            // Read the slave device path before forking (ptsname has a static
            // buffer on some platforms, so we copy it now).
            let slave = slave_name(master.0)?;

            // Set the initial terminal size.
            let ws =
                libc::winsize { ws_row: height, ws_col: width, ws_xpixel: 0, ws_ypixel: 0 };
            libc::ioctl(master.0, libc::TIOCSWINSZ, &ws);

            // Duplicate master for the reader thread; also O_CLOEXEC.
            let reader_fd = check_ret(libc::dup(master.0))?;
            set_cloexec(reader_fd);
            let reader_fd = OwnedFd(reader_fd);

            let pid = check_ret(libc::fork())?;

            if pid == 0 {
                // Child: configure the terminal, then exec.  Never returns.
                child_exec(master.0, &slave, &argv, cwd_c.as_deref());
            }

            // Parent.
            Ok((
                Pty {
                    master_fd: master.take(),
                    child_pid: pid as libc::pid_t,
                    cached_exit: Cell::new(None),
                },
                PtyReader { master_fd: reader_fd.take() },
            ))
        }
    }

    /// Writes to the child's stdin.
    pub fn write(&self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let ret =
                unsafe { libc::write(self.master_fd, data.as_ptr().cast(), data.len()) };
            if ret < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            data = &data[ret as usize..];
        }
        Ok(())
    }

    /// Tells the child that the window size changed.
    pub fn resize(&self, size: Size) -> io::Result<()> {
        let width = size.width.clamp(1, u16::MAX as CoordType) as u16;
        let height = size.height.clamp(1, u16::MAX as CoordType) as u16;
        let ws = libc::winsize { ws_row: height, ws_col: width, ws_xpixel: 0, ws_ypixel: 0 };
        let ret = unsafe { libc::ioctl(self.master_fd, libc::TIOCSWINSZ, &ws) };
        if ret < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// Returns the exit code, or `None` if the child is still running.
    ///
    /// Once this returns `Some`, the result is cached; subsequent calls return
    /// the same value without calling `waitpid` again.
    pub fn try_exit_code(&self) -> Option<u32> {
        if let Some(code) = self.cached_exit.get() {
            return Some(code);
        }
        let mut status = 0i32;
        let ret = unsafe { libc::waitpid(self.child_pid, &mut status, libc::WNOHANG) };
        let code = if ret == self.child_pid {
            if libc::WIFEXITED(status) {
                Some(libc::WEXITSTATUS(status) as u32)
            } else if libc::WIFSIGNALED(status) {
                Some(128 + libc::WTERMSIG(status) as u32)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(c) = code {
            self.cached_exit.set(Some(c));
        }
        code
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe {
            if self.cached_exit.get().is_none() {
                // Child wasn't reaped yet; kill it and wait.
                libc::kill(self.child_pid, libc::SIGKILL);
                let mut status = 0i32;
                libc::waitpid(self.child_pid, &mut status, 0);
            }
            // cached_exit is Some → child was already reaped by try_exit_code.
            libc::close(self.master_fd);
        }
    }
}

impl PtyReader {
    /// Blocks until the child produces output.
    ///
    /// Returns `Ok(0)` on EOF (slave side of the PTY closed — the child exited
    /// and all its open fds were closed).
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let ret =
                unsafe { libc::read(self.master_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if ret >= 0 {
                return Ok(ret as usize);
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EINTR) => continue,
                // EIO: the slave side is gone — treat as EOF.
                Some(libc::EIO) => return Ok(0),
                _ => return Err(err),
            }
        }
    }
}

impl Drop for PtyReader {
    fn drop(&mut self) {
        unsafe { libc::close(self.master_fd) };
    }
}

/// Returns the command line to use when the user didn't configure one.
pub fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn check_ret(ret: c_int) -> io::Result<c_int> {
    if ret < 0 { Err(io::Error::last_os_error()) } else { Ok(ret) }
}

fn set_cloexec(fd: c_int) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

/// Returns the slave PTY device path as a `CString`.
fn slave_name(master_fd: c_int) -> io::Result<CString> {
    #[cfg(target_os = "linux")]
    {
        let mut buf = [0u8; 256];
        let ret = unsafe { libc::ptsname_r(master_fd, buf.as_mut_ptr().cast(), buf.len()) };
        if ret != 0 {
            return Err(io::Error::from_raw_os_error(ret));
        }
        let cstr = unsafe { CStr::from_ptr(buf.as_ptr().cast()) };
        Ok(cstr.to_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let ptr = unsafe { libc::ptsname(master_fd) };
        if ptr.is_null() {
            return Err(io::Error::last_os_error());
        }
        // Copy immediately: the pointer is into a static buffer.
        let cstr = unsafe { CStr::from_ptr(ptr) };
        Ok(cstr.to_owned())
    }
}

/// Parses `command` by ASCII-whitespace splitting into a non-empty argv.
fn parse_command(command: &str) -> io::Result<Vec<CString>> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    }
    parts
        .into_iter()
        .map(|s| {
            CString::new(s).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "null byte in command argument",
                )
            })
        })
        .collect()
}

/// Runs entirely inside the child process after `fork`.
///
/// Sets up the slave PTY as the controlling terminal, redirects stdio,
/// optionally changes directory, and execs the command.  Calls `_exit(1)` on
/// any error so it never returns normally.
unsafe fn child_exec(
    master_fd: c_int,
    slave_name: &CStr,
    argv: &[CString],
    cwd: Option<&CStr>,
) -> ! {
    // New session: we have no controlling terminal yet.
    libc::setsid();

    // Open the slave side of the PTY.
    let slave = libc::open(slave_name.as_ptr(), libc::O_RDWR);
    if slave < 0 {
        libc::_exit(1);
    }

    // Acquire the slave as our controlling terminal.
    // On Linux the second arg is a "steal" flag (0 = don't steal).
    // On macOS/BSD the arg is ignored.
    libc::ioctl(slave, libc::TIOCSCTTY as _, 0i32);

    // Redirect stdio to the slave PTY.
    if libc::dup2(slave, libc::STDIN_FILENO) < 0 {
        libc::_exit(1);
    }
    if libc::dup2(slave, libc::STDOUT_FILENO) < 0 {
        libc::_exit(1);
    }
    if libc::dup2(slave, libc::STDERR_FILENO) < 0 {
        libc::_exit(1);
    }

    // Close the original slave fd now that it's been dup2'd to 0/1/2.
    if slave > libc::STDERR_FILENO {
        libc::close(slave);
    }

    // master_fd has O_CLOEXEC so execvp will close it automatically, but
    // close it explicitly here to keep things tidy before the exec.
    libc::close(master_fd);

    // Change working directory if requested.
    if let Some(dir) = cwd {
        // Ignore errors: the shell will report them if the dir is wrong.
        libc::chdir(dir.as_ptr());
    }

    // Build a null-terminated pointer array for execvp.
    let mut ptrs: Vec<*const libc::c_char> = argv.iter().map(|s| s.as_ptr()).collect();
    ptrs.push(std::ptr::null());

    libc::execvp(argv[0].as_ptr(), ptrs.as_ptr());
    // exec failed (command not found, permission denied, …).
    libc::_exit(127);
}

/// RAII wrapper that closes a file descriptor on drop.
struct OwnedFd(c_int);

impl OwnedFd {
    /// Takes the fd out of the wrapper without closing it.
    fn take(&mut self) -> c_int {
        let fd = self.0;
        self.0 = -1;
        fd
    }
}

impl Drop for OwnedFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe { libc::close(self.0) };
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    const SIZE: Size = Size { width: 80, height: 25 };

    /// Spawns a reader thread that drains the PTY until EOF.
    fn drain_on_thread(mut reader: PtyReader) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 4 * KIBI];
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
        rx
    }

    /// Collects output until `marker` appears in the accumulated bytes.
    fn wait_for_marker(rx: &mpsc::Receiver<Vec<u8>>, marker: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        while Instant::now() < deadline {
            let Ok(chunk) = rx.recv_timeout(Duration::from_millis(250)) else {
                continue;
            };
            output.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&output).contains(marker) {
                return String::from_utf8_lossy(&output).into_owned();
            }
        }
        panic!("never saw {marker:?} in: {:?}", String::from_utf8_lossy(&output));
    }

    fn wait_for_exit(pty: &Pty) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(code) = pty.try_exit_code() {
                return code;
            }
            assert!(Instant::now() < deadline, "child never exited");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn spawn_echoes_output() {
        let (pty, reader) = Pty::spawn("sh", None, SIZE).unwrap();
        let rx = drain_on_thread(reader);
        pty.write(b"echo hello-from-pty\n").unwrap();
        wait_for_marker(&rx, "hello-from-pty");
    }

    #[test]
    fn reports_exit_code() {
        // Send the exit command interactively so we don't need shell quoting.
        let (pty, reader) = Pty::spawn("sh", None, SIZE).unwrap();
        let rx = drain_on_thread(reader);
        pty.write(b"exit 42\n").unwrap();
        assert_eq!(wait_for_exit(&pty), 42);
        drop(pty);
        while rx.recv_timeout(Duration::from_secs(5)).is_ok() {}
    }

    #[test]
    fn write_reaches_the_child() {
        let (pty, reader) = Pty::spawn("sh", None, SIZE).unwrap();
        let rx = drain_on_thread(reader);
        pty.write(b"echo round-trip-marker\n").unwrap();
        wait_for_marker(&rx, "round-trip-marker");
    }

    #[test]
    fn resize_is_accepted() {
        let (pty, reader) = Pty::spawn("sh", None, SIZE).unwrap();
        let rx = drain_on_thread(reader);
        pty.resize(Size { width: 120, height: 40 }).unwrap();
        pty.resize(Size { width: 0, height: 0 }).unwrap(); // clamped to 1×1
        drop(pty);
        while rx.recv_timeout(Duration::from_secs(5)).is_ok() {}
    }
}
