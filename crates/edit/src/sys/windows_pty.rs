// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pseudo console (ConPTY) support.
//!
//! This spawns a child process attached to a pseudo console, so that we can
//! host a fully interactive terminal inside the editor. The child talks VT to
//! us just like we talk VT to the terminal that hosts the editor.
//!
//! # Handle ownership
//!
//! The read end of the output pipe is split off into a [`PtyReader`], because
//! it's the only part that gets moved to another thread. Everything else stays
//! on the main thread inside [`Pty`].
//!
//! [`Pty`] deliberately does **not** join the reader thread on drop.
//! `ClosePseudoConsole` blocks until the pseudo console flushed its pending
//! output, which can only happen if someone keeps draining the pipe. Joining
//! first would deadlock; instead the reader thread notices the EOF on its own
//! and exits.

use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::{io, mem};

use windows_sys::Win32::Foundation;
use windows_sys::Win32::Storage::FileSystem;
use windows_sys::Win32::System::{Console, JobObjects, Pipes, Threading};

use super::windows::{check_bool_return, last_os_error};
use crate::helpers::*;

/// The value `GetExitCodeProcess` reports while the process is still running.
const STILL_ACTIVE: u32 = 259;

/// Every child is put in this job, which is configured to kill its members
/// when the last handle to it closes. Closing a [`Pty`] normally terminates its
/// own child, but that only runs if we actually get to run: if the editor is
/// killed or crashes, the handle goes away with the process and the kernel
/// cleans up the shells for us. Without it they'd keep running invisibly.
///
/// The job is created once and never closed, so its lifetime is the process's.
static mut JOB: Foundation::HANDLE = null_mut();
static JOB_INIT: std::sync::Once = std::sync::Once::new();

fn job() -> Foundation::HANDLE {
    JOB_INIT.call_once(|| unsafe {
        let handle = JobObjects::CreateJobObjectW(null(), null());
        if handle.is_null() {
            return;
        }

        let mut limits: JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        let ok = JobObjects::SetInformationJobObject(
            handle,
            JobObjects::JobObjectExtendedLimitInformation,
            &raw const limits as *const c_void,
            mem::size_of::<JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );

        if ok == 0 {
            // A job we can't configure is worse than none: it would still trap
            // the children without the guarantee that they get cleaned up.
            Foundation::CloseHandle(handle);
            return;
        }

        JOB = handle;
    });

    unsafe { JOB }
}

/// A child process attached to a pseudo console.
pub struct Pty {
    hpc: Console::HPCON,
    /// Write end of the input pipe. Anything we write here becomes child stdin.
    input: Foundation::HANDLE,
    process: Foundation::HANDLE,
    thread: Foundation::HANDLE,
}

/// The read end of a [`Pty`]'s output pipe.
///
/// This is the only piece that gets moved onto the reader thread.
pub struct PtyReader {
    output: Foundation::HANDLE,
}

// SAFETY: A `HANDLE` is just a kernel object index. It has no thread affinity
// and `ReadFile` is safe to call from any thread. `PtyReader` has exclusive
// ownership of the handle, so no other thread can close it from under us.
unsafe impl Send for PtyReader {}

impl Pty {
    /// Spawns `command` attached to a new pseudo console of the given size.
    ///
    /// `command` is a full command line, not just an executable path,
    /// because that's what `CreateProcessW` wants anyway.
    pub fn spawn(
        command: &str,
        cwd: Option<&Path>,
        size: Size,
    ) -> io::Result<(Pty, PtyReader)> {
        // A zero-sized pseudo console makes ConPTY very unhappy.
        let width = size.width.clamp(1, i16::MAX as CoordType) as i16;
        let height = size.height.clamp(1, i16::MAX as CoordType) as i16;

        unsafe {
            // We write `input`, the pseudo console reads `input_read`.
            let mut input_read = null_mut();
            let mut input = null_mut();
            check_bool_return(Pipes::CreatePipe(&mut input_read, &mut input, null(), 0))?;
            let input_read = OwnedHandle(input_read);
            let mut input = OwnedHandle(input);

            // The pseudo console writes `output_write`, we read `output`.
            let mut output = null_mut();
            let mut output_write = null_mut();
            check_bool_return(Pipes::CreatePipe(&mut output, &mut output_write, null(), 0))?;
            let mut output = OwnedHandle(output);
            let output_write = OwnedHandle(output_write);

            let mut hpc: Console::HPCON = 0;
            check_hresult(Console::CreatePseudoConsole(
                Console::COORD { X: width, Y: height },
                input_read.0,
                output_write.0,
                0,
                &mut hpc,
            ))?;
            let mut hpc = OwnedPseudoConsole(hpc);

            // The pseudo console duplicated both handles. If we kept our copies
            // around, we'd never see an EOF once the child goes away.
            drop(input_read);
            drop(output_write);

            // Ask for the size of the attribute list, then allocate it.
            // The first call is expected to "fail" with ERROR_INSUFFICIENT_BUFFER.
            let mut attribute_list_size = 0;
            Threading::InitializeProcThreadAttributeList(
                null_mut(),
                1,
                0,
                &mut attribute_list_size,
            );
            if attribute_list_size == 0 {
                return Err(last_os_error());
            }

            // `PROC_THREAD_ATTRIBUTE_LIST` needs pointer alignment,
            // which a `Vec<u8>` wouldn't give us.
            let mut attribute_list =
                vec![0usize; attribute_list_size.div_ceil(mem::size_of::<usize>())];
            let attribute_list = attribute_list.as_mut_ptr() as *mut c_void;

            check_bool_return(Threading::InitializeProcThreadAttributeList(
                attribute_list,
                1,
                0,
                &mut attribute_list_size,
            ))?;
            let attribute_list = OwnedAttributeList(attribute_list);

            // NOTE: `lpValue` *is* the `HPCON`, not a pointer to one, even though
            // `cbSize` is `sizeof(HPCON)`. Passing `&hpc` type-checks fine and
            // every call still reports success -- the child then dies during
            // startup with STATUS_DLL_INIT_FAILED.
            check_bool_return(Threading::UpdateProcThreadAttribute(
                attribute_list.0,
                0,
                Threading::PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc.0 as *const c_void,
                mem::size_of::<Console::HPCON>(),
                null_mut(),
                null(),
            ))?;

            let mut startup_info: Threading::STARTUPINFOEXW = mem::zeroed();
            startup_info.StartupInfo.cb =
                mem::size_of::<Threading::STARTUPINFOEXW>() as u32;
            startup_info.lpAttributeList = attribute_list.0;

            // Without this, the child keeps *our* std handles and happily writes
            // its output to the terminal the editor is running in, while the
            // pseudo console sits there with a client that never draws anything.
            // Handing it `INVALID_HANDLE_VALUE` forces it onto the pseudo console.
            startup_info.StartupInfo.dwFlags = Threading::STARTF_USESTDHANDLES;
            startup_info.StartupInfo.hStdInput = Foundation::INVALID_HANDLE_VALUE;
            startup_info.StartupInfo.hStdOutput = Foundation::INVALID_HANDLE_VALUE;
            startup_info.StartupInfo.hStdError = Foundation::INVALID_HANDLE_VALUE;

            // `CreateProcessW` may modify the command line in place, so it has to
            // live in a buffer we own.
            let mut command = to_utf16(command);
            let cwd = cwd.map(|p| to_utf16(&p.to_string_lossy()));
            let cwd = cwd.as_ref().map_or(null(), |c| c.as_ptr());

            let mut process_info: Threading::PROCESS_INFORMATION = mem::zeroed();
            check_bool_return(Threading::CreateProcessW(
                null(),
                command.as_mut_ptr(),
                null(),
                null(),
                0, // Don't inherit handles: the pseudo console has its own copies.
                Threading::EXTENDED_STARTUPINFO_PRESENT,
                null(),
                cwd,
                &startup_info.StartupInfo,
                &mut process_info,
            ))?;

            // Best effort: a child outside the job still works, it just won't
            // be cleaned up if the editor dies without running its teardown.
            let job = job();
            if !job.is_null() {
                JobObjects::AssignProcessToJobObject(job, process_info.hProcess);
            }

            Ok((
                Pty {
                    hpc: hpc.take(),
                    input: input.take(),
                    process: process_info.hProcess,
                    thread: process_info.hThread,
                },
                PtyReader { output: output.take() },
            ))
        }
    }

    /// Writes to the child's stdin.
    pub fn write(&self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let mut written = 0;
            let ok = unsafe {
                FileSystem::WriteFile(
                    self.input,
                    data.as_ptr(),
                    data.len().min(GIBI) as u32,
                    &mut written,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(last_os_error());
            }
            if written == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            data = &data[written as usize..];
        }
        Ok(())
    }

    /// Tells the child that the window size changed.
    pub fn resize(&self, size: Size) -> io::Result<()> {
        let width = size.width.clamp(1, i16::MAX as CoordType) as i16;
        let height = size.height.clamp(1, i16::MAX as CoordType) as i16;
        unsafe {
            check_hresult(Console::ResizePseudoConsole(
                self.hpc,
                Console::COORD { X: width, Y: height },
            ))
        }
    }

    /// Returns the exit code, or `None` if the child is still running.
    pub fn try_exit_code(&self) -> Option<u32> {
        unsafe {
            let mut code = 0;
            if Threading::GetExitCodeProcess(self.process, &mut code) == 0
                || code == STILL_ACTIVE
            {
                None
            } else {
                Some(code)
            }
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe {
            // If the child is still around, it won't go away on its own just
            // because we closed the pseudo console. Some shells hold on for a
            // surprisingly long time, so don't ask nicely.
            if self.try_exit_code().is_none() {
                Threading::TerminateProcess(self.process, 0);
            }

            // NOTE: This blocks until the pseudo console flushed its output.
            // See the module docs for why we must not join the reader first.
            Console::ClosePseudoConsole(self.hpc);

            Foundation::CloseHandle(self.input);
            Foundation::CloseHandle(self.thread);
            Foundation::CloseHandle(self.process);
        }
    }
}

impl PtyReader {
    /// Blocks until the child produces output.
    ///
    /// Returns `Ok(0)` on EOF.
    ///
    /// **NOTE:** EOF does *not* happen when the child exits. The pseudo console
    /// owns the write end of this pipe and keeps it open for as long as it
    /// lives, so this blocks indefinitely until [`Pty`] is dropped. Use
    /// [`Pty::try_exit_code`] to notice that the child is gone.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        unsafe {
            let mut read = 0;
            let ok = FileSystem::ReadFile(
                self.output,
                buf.as_mut_ptr(),
                buf.len().min(GIBI) as u32,
                &mut read,
                null_mut(),
            );
            if ok == 0 {
                // The child exited and the pseudo console closed its end.
                // That's an orderly EOF as far as we're concerned.
                let err = last_os_error();
                return match err.raw_os_error() {
                    Some(code)
                        if code == Foundation::ERROR_BROKEN_PIPE as i32
                            || code == Foundation::ERROR_HANDLE_EOF as i32 =>
                    {
                        Ok(0)
                    }
                    _ => Err(err),
                };
            }
            Ok(read as usize)
        }
    }
}

impl Drop for PtyReader {
    fn drop(&mut self) {
        unsafe { Foundation::CloseHandle(self.output) };
    }
}

/// Returns the command line to use when the user didn't configure one.
pub fn default_shell() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
}

fn to_utf16(s: &str) -> Vec<u16> {
    let mut buf: Vec<u16> = OsStr::new(s).encode_wide().collect();
    buf.push(0);
    buf
}

fn check_hresult(hr: windows_sys::core::HRESULT) -> io::Result<()> {
    if hr >= 0 {
        Ok(())
    } else if (hr as u32) >> 16 == 0x8007 {
        // HRESULT_FROM_WIN32: the low word is a plain Win32 error code.
        Err(io::Error::from_raw_os_error(hr & 0xffff))
    } else {
        Err(io::Error::other(format!("HRESULT 0x{:08x}", hr as u32)))
    }
}

/// Closes a handle on drop, so that the error paths in
/// [`Pty::spawn`] don't have to unwind by hand.
struct OwnedHandle(Foundation::HANDLE);

impl OwnedHandle {
    fn take(&mut self) -> Foundation::HANDLE {
        mem::replace(&mut self.0, null_mut())
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { Foundation::CloseHandle(self.0) };
        }
    }
}

struct OwnedPseudoConsole(Console::HPCON);

impl OwnedPseudoConsole {
    fn take(&mut self) -> Console::HPCON {
        mem::replace(&mut self.0, 0)
    }
}

impl Drop for OwnedPseudoConsole {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe { Console::ClosePseudoConsole(self.0) };
        }
    }
}

struct OwnedAttributeList(*mut c_void);

impl Drop for OwnedAttributeList {
    fn drop(&mut self) {
        unsafe { Threading::DeleteProcThreadAttributeList(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use std::thread;

    use super::*;

    const SIZE: Size = Size { width: 80, height: 25 };

    /// Mirrors how the editor uses this: the reader lives on its own thread and
    /// streams whatever it gets until the pseudo console goes away.
    fn drain_on_thread(mut reader: PtyReader) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0; 4 * KIBI];
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

    /// Collects output until `marker` shows up.
    ///
    /// We can't just wait for the child to exit and then read what's there:
    /// ConPTY renders asynchronously, so closing it right after the child exits
    /// races the renderer and drops the tail of the output.
    fn wait_for_marker(rx: &mpsc::Receiver<Vec<u8>>, marker: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
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
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(code) = pty.try_exit_code() {
                return code;
            }
            assert!(Instant::now() < deadline, "the child never exited");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn spawn_echoes_output() {
        // `cmd /c echo` exits immediately, so keep the shell alive instead:
        // we need it around long enough for ConPTY to render the text.
        let (pty, reader) = Pty::spawn("cmd.exe", None, SIZE).unwrap();
        let rx = drain_on_thread(reader);

        pty.write(b"echo hello-from-conpty\r\n").unwrap();
        wait_for_marker(&rx, "hello-from-conpty");
    }

    #[test]
    fn reports_exit_code() {
        let (pty, reader) = Pty::spawn("cmd.exe /c exit 42", None, SIZE).unwrap();
        let rx = drain_on_thread(reader);

        assert_eq!(wait_for_exit(&pty), 42);

        drop(pty);
        // The reader must see EOF once the pseudo console is gone.
        while rx.recv_timeout(Duration::from_secs(30)).is_ok() {}
    }

    #[test]
    fn write_reaches_the_child() {
        let (pty, reader) = Pty::spawn("cmd.exe", None, SIZE).unwrap();
        let rx = drain_on_thread(reader);

        pty.write(b"echo round-trip-marker\r\n").unwrap();
        wait_for_marker(&rx, "round-trip-marker");
    }

    #[test]
    fn resize_is_accepted() {
        let (pty, reader) = Pty::spawn("cmd.exe", None, SIZE).unwrap();
        let rx = drain_on_thread(reader);

        pty.resize(Size { width: 120, height: 40 }).unwrap();
        // A zero size must not be passed through to ConPTY.
        pty.resize(Size { width: 0, height: 0 }).unwrap();

        drop(pty); // Terminates the still-running shell.

        // Draining to the end proves the reader gets its EOF rather than
        // hanging forever on a pseudo console that's already gone.
        while rx.recv_timeout(Duration::from_secs(30)).is_ok() {}
    }
}
