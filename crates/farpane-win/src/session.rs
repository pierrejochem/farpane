//! Starting the session worker in the user's session (Windows only).
//!
//! The service runs as `LocalSystem` in session 0, which has no screen. This
//! module finds the session on the console, copies the service's own token
//! into that session, and starts `farpane-server worker` with it. SYSTEM is
//! deliberate: only a SYSTEM process may attach to the secure desktop (lock
//! screen, sign-in, UAC), see [`crate::desktop`]. The worker's standard input
//! and output are two anonymous pipes created here, with only the child's ends
//! inheritable, so no other process can reach it or the service through them.
//!
//! [`supervise`] keeps one worker alive for the console session and swaps it
//! when the console moves to another session (fast user switching) or the
//! worker dies.

use std::{
    ffi::OsString,
    fs::File,
    io,
    os::windows::{ffi::OsStrExt, io::FromRawHandle},
    path::Path,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
            WAIT_OBJECT_0,
        },
        Security::{
            DuplicateTokenEx, SECURITY_ATTRIBUTES, SecurityImpersonation, SetTokenInformation,
            TOKEN_ACCESS_MASK, TOKEN_ALL_ACCESS, TokenPrimary, TokenSessionId,
        },
        System::{
            Pipes::CreatePipe,
            RemoteDesktop::WTSGetActiveConsoleSessionId,
            Threading::{
                CREATE_NO_WINDOW, CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess,
                OpenProcessToken, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
                TerminateProcess, WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

use farpane_session::remote::Hub;

/// `WTSGetActiveConsoleSessionId` when nobody is on the console.
const NO_SESSION: u32 = 0xFFFF_FFFF;
/// How often the console session and the worker are looked at.
const POLL: Duration = Duration::from_millis(500);
/// Pause before starting a worker again after one ended.
const RESTART_DELAY: Duration = Duration::from_secs(2);

/// An owned kernel handle, closed on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this struct and closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: a kernel handle value may be used and closed from any thread.
unsafe impl Send for Handle {}

/// A running worker process.
struct Worker {
    process: Handle,
    /// Frames come in on the first, commands go out on the second; taken
    /// once, when the worker is handed to the hub.
    pipes: Option<(File, File)>,
}

impl Worker {
    fn exited(&self) -> bool {
        // SAFETY: the process handle is valid for the life of `self`.
        let state = unsafe { WaitForSingleObject(self.process.0, 0) };
        state == WAIT_OBJECT_0
    }

    fn exit_code(&self) -> Option<u32> {
        let mut code = 0u32;
        // SAFETY: valid handle and a valid out pointer.
        unsafe { GetExitCodeProcess(self.process.0, &raw mut code) }
            .ok()
            .map(|()| code)
    }

    fn kill(&self) {
        // SAFETY: valid handle; failure means it had ended already.
        let _ = unsafe { TerminateProcess(self.process.0, 1) };
    }
}

/// The session on the physical console, if anyone (or the sign-in screen) is there.
#[must_use]
pub fn console_session() -> Option<u32> {
    // SAFETY: no preconditions.
    let id = unsafe { WTSGetActiveConsoleSessionId() };
    (id != NO_SESSION).then_some(id)
}

/// A pipe whose `child` end can be inherited and whose other end cannot.
/// Returns (read end, write end); `inherit_read` says which one the child gets.
fn pipe(inherit_read: bool) -> io::Result<(Handle, Handle)> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(24),
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    // SAFETY: the out pointers are valid and the attributes live through the call.
    unsafe {
        CreatePipe(
            &raw mut read,
            &raw mut write,
            Some(&raw const attributes),
            1 << 20,
        )
    }
    .map_err(io::Error::other)?;
    let (read, write) = (Handle(read), Handle(write));
    let ours = if inherit_read { write.0 } else { read.0 };
    // SAFETY: valid handle; clears the inherit flag so the child never gets this end.
    unsafe { SetHandleInformation(ours, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(io::Error::other)?;
    Ok((read, write))
}

/// Our own token, copied into `session`.
fn token_for(session: u32) -> io::Result<Handle> {
    let mut own = HANDLE::default();
    // SAFETY: the pseudo handle of this process is always valid; `own` is a valid out pointer.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &raw mut own) }
        .map_err(io::Error::other)?;
    let own = Handle(own);
    let mut copy = HANDLE::default();
    // SAFETY: valid token handle and out pointer; 0x0200_0000 is MAXIMUM_ALLOWED.
    unsafe {
        DuplicateTokenEx(
            own.0,
            TOKEN_ACCESS_MASK(0x0200_0000),
            None,
            SecurityImpersonation,
            TokenPrimary,
            &raw mut copy,
        )
    }
    .map_err(io::Error::other)?;
    let copy = Handle(copy);
    // SAFETY: `session` outlives the call and the length is that of a u32.
    unsafe { SetTokenInformation(copy.0, TokenSessionId, ptr::from_ref(&session).cast(), 4) }
        .map_err(io::Error::other)?;
    Ok(copy)
}

/// Start `exe arguments...` in `session` with the service's rights.
fn spawn(session: u32, exe: &Path, arguments: &[OsString]) -> io::Result<Worker> {
    let token = token_for(session)?;
    // The child reads commands from stdin and writes frames to stdout.
    let (child_stdin, our_commands) = pipe(true)?;
    let (our_frames, child_stdout) = pipe(false)?;

    let mut line: Vec<u16> = Vec::new();
    for word in std::iter::once(exe.as_os_str().to_owned()).chain(arguments.iter().cloned()) {
        if !line.is_empty() {
            line.push(u16::from(b' '));
        }
        line.push(u16::from(b'"'));
        line.extend(word.encode_wide());
        line.push(u16::from(b'"'));
    }
    line.push(0);
    let mut desktop: Vec<u16> = "winsta0\\default\0".encode_utf16().collect();
    let startup = STARTUPINFOW {
        cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap_or(104),
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        dwFlags: STARTF_USESTDHANDLES,
        hStdInput: child_stdin.0,
        hStdOutput: child_stdout.0,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: the token, the pipe ends and both wide strings are valid for the
    // call (the command line is writable as the API requires); `info` receives
    // two handles that are closed below.
    unsafe {
        CreateProcessAsUserW(
            Some(token.0),
            PCWSTR::null(),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &raw const startup,
            &raw mut info,
        )
    }
    .map_err(io::Error::other)?;
    drop(Handle(info.hThread));
    let process = Handle(info.hProcess);
    // The child has its own copies now; ours must go so the pipes break when it ends.
    drop((child_stdin, child_stdout));
    // SAFETY: each handle is a pipe end owned by this function; `File` takes
    // over closing it, and the `Handle` wrappers are forgotten so it is not closed twice.
    let (from, to) = unsafe {
        let from = File::from_raw_handle(our_frames.0.0);
        let to = File::from_raw_handle(our_commands.0.0);
        (from, to)
    };
    std::mem::forget(our_frames);
    std::mem::forget(our_commands);
    Ok(Worker {
        process,
        pipes: Some((from, to)),
    })
}

/// Keep a worker running in the console session until `stop` is set, handing
/// each one to `hub`. `log` receives a line for each start, change and end.
/// Blocks; run it on a thread of its own.
pub fn supervise(
    hub: &Hub,
    exe: &Path,
    arguments: &[OsString],
    stop: &Arc<AtomicBool>,
    log: &dyn Fn(&str),
) {
    while !stop.load(Ordering::Relaxed) {
        let Some(session) = console_session() else {
            thread::sleep(POLL);
            continue;
        };
        let mut worker = match spawn(session, exe, arguments) {
            Ok(w) => w,
            Err(e) => {
                log(&format!(
                    "cannot start the session worker in session {session}: {e}"
                ));
                thread::sleep(RESTART_DELAY);
                continue;
            }
        };
        log(&format!("session worker started in session {session}"));
        if let Some((from, to)) = worker.pipes.take() {
            hub.attach(from, to);
        }
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(POLL);
            if worker.exited() {
                log(&format!(
                    "session worker ended (exit code {})",
                    worker
                        .exit_code()
                        .map_or_else(|| "unknown".into(), |c| c.to_string())
                ));
                break;
            }
            if console_session() != Some(session) {
                log("the console moved to another session; restarting the worker");
                break;
            }
        }
        hub.detach();
        worker.kill();
        if !stop.load(Ordering::Relaxed) {
            thread::sleep(RESTART_DELAY);
        }
    }
}
