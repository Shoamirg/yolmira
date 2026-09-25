//! Process creation with explicit flags, environment and job membership.
//! `std::process::Command` can't create a suspended process or report breakaway
//! failures precisely, both of which the launcher and job handling need.

use super::{last_error, wide, OwnedHandle};
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use windows_sys::Win32::Foundation::{STILL_ACTIVE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, GetExitCodeProcess, OpenProcess, ResumeThread,
    TerminateProcess, WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    PROCESS_INFORMATION, PROCESS_SYNCHRONIZE, STARTUPINFOW,
};

pub use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
};

#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// `None` inherits the caller's environment; `Some` passes exactly these variables.
    pub env: Option<Vec<(String, String)>>,
    pub cwd: Option<PathBuf>,
    /// `CREATE_*` / `DETACHED_PROCESS` flags. `CREATE_UNICODE_ENVIRONMENT` is added as needed.
    pub flags: u32,
}

#[derive(Debug)]
pub struct Process {
    handle: OwnedHandle,
    thread: OwnedHandle,
    pid: u32,
}

impl Process {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Exit code if the process has exited within `timeout`.
    pub fn wait(&self, timeout: Duration) -> io::Result<Option<u32>> {
        wait_handle(&self.handle, timeout)?;
        self.exit_code()
    }

    /// `None` while running.
    pub fn exit_code(&self) -> io::Result<Option<u32>> {
        let mut code = 0u32;
        // SAFETY: valid process handle; `code` is an out-param.
        if unsafe { GetExitCodeProcess(self.handle.raw(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((code != STILL_ACTIVE as u32).then_some(code))
    }

    pub fn resume(&self) -> io::Result<()> {
        // SAFETY: valid thread handle of a process we created.
        if unsafe { ResumeThread(self.thread.raw()) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: valid process handle.
        if unsafe { TerminateProcess(self.handle.raw(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Create a process. On failure the `io::Error` carries the Win32 code (e.g. 5 for a
/// breakaway the caller's job forbids).
pub fn spawn(opts: &SpawnOptions) -> io::Result<Process> {
    let mut cmdline = wide(command_line(&opts.exe.to_string_lossy(), &opts.args));
    let env_block = opts.env.as_ref().map(|vars| env_block(vars));
    let cwd = opts.cwd.as_ref().map(wide);
    let flags = opts.flags
        | if env_block.is_some() {
            CREATE_UNICODE_ENVIRONMENT
        } else {
            0
        };
    let mut si: STARTUPINFOW = unsafe_zeroed();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe_zeroed();
    let exe = wide(&opts.exe);
    // SAFETY: all pointers are valid NUL-terminated buffers that outlive the call;
    // `cmdline` is mutable as CreateProcessW requires; handles are not inherited.
    let ok = unsafe {
        CreateProcessW(
            exe.as_ptr(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            flags,
            env_block
                .as_ref()
                .map_or(std::ptr::null(), |b| b.as_ptr().cast()),
            cwd.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::from_raw_os_error(last_error() as i32));
    }
    let handle =
        OwnedHandle::new(pi.hProcess).ok_or_else(|| io::Error::other("no process handle"))?;
    let thread =
        OwnedHandle::new(pi.hThread).ok_or_else(|| io::Error::other("no thread handle"))?;
    Ok(Process {
        handle,
        thread,
        pid: pi.dwProcessId,
    })
}

fn unsafe_zeroed<T>() -> T {
    // SAFETY: only used for plain-old-data Win32 structs whose all-zero value is valid.
    unsafe { std::mem::zeroed() }
}

/// Quote per the MSVC argv rules so `CommandLineToArgvW` gives back `args` exactly.
pub fn command_line(exe: &str, args: &[String]) -> String {
    std::iter::once(exe)
        .chain(args.iter().map(String::as_str))
        .map(quote_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// `k=v\0…\0\0`, sorted case-insensitively as Windows expects.
fn env_block(vars: &[(String, String)]) -> Vec<u16> {
    let mut sorted: Vec<_> = vars.iter().collect();
    sorted.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block: Vec<u16> = sorted
        .iter()
        .flat_map(|(k, v)| {
            format!("{k}={v}")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>()
        })
        .collect();
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// A Job Object. With `kill_on_close`, closing it kills every process still in it.
pub struct Job {
    handle: OwnedHandle,
}

impl Job {
    pub fn new(kill_on_close: bool, allow_breakaway: bool) -> io::Result<Self> {
        // SAFETY: anonymous job, default security.
        let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let handle = OwnedHandle::new(h).ok_or_else(io::Error::last_os_error)?;
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe_zeroed();
        info.BasicLimitInformation.LimitFlags = (if kill_on_close {
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        } else {
            0
        }) | (if allow_breakaway {
            JOB_OBJECT_LIMIT_BREAKAWAY_OK
        } else {
            0
        });
        // SAFETY: `info` is the struct JobObjectExtendedLimitInformation expects, with its size.
        let ok = unsafe {
            SetInformationJobObject(
                handle.raw(),
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// Spawn suspended, assign to this job, then resume: the process can't start
    /// children outside the job before it is assigned (ARCHITECTURE §9.6).
    pub fn spawn(&self, opts: &SpawnOptions) -> io::Result<Process> {
        let suspended = SpawnOptions {
            flags: opts.flags | CREATE_SUSPENDED,
            ..opts.clone()
        };
        let p = spawn(&suspended)?;
        // SAFETY: both handles are valid.
        if unsafe { AssignProcessToJobObject(self.handle.raw(), p.handle.raw()) } == 0 {
            return Err(kill_after(&p, io::Error::last_os_error()));
        }
        if let Err(e) = p.resume() {
            return Err(kill_after(&p, e));
        }
        Ok(p)
    }
}

/// A suspended process we can't use must not linger; report both failures if killing fails.
fn kill_after(p: &Process, cause: io::Error) -> io::Error {
    match p.kill() {
        Ok(()) => cause,
        Err(k) => io::Error::other(format!(
            "{cause}; and killing the suspended process failed: {k}"
        )),
    }
}

/// `true` if the handle was signaled within `timeout`; errors on WAIT_FAILED.
fn wait_handle(h: &OwnedHandle, timeout: Duration) -> io::Result<bool> {
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: valid handle owned by the caller.
    match unsafe { WaitForSingleObject(h.raw(), ms) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

/// Wait for process `pid` to exit. `Ok(true)` if it exited (or no longer exists).
pub fn wait_pid_exit(pid: u32, timeout: Duration) -> io::Result<bool> {
    // SAFETY: plain open; owned below.
    let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    match OwnedHandle::new(h) {
        Some(h) => wait_handle(&h, timeout),
        // ERROR_INVALID_PARAMETER: no such process any more.
        None if last_error() == 87 => Ok(true),
        None => Err(io::Error::last_os_error()),
    }
}

/// A handle to another process, held from right after it starts (so its pid can't be
/// reused under us), for waiting on its exit and reading the exit code.
#[derive(Debug)]
pub struct ProcessWatch {
    handle: OwnedHandle,
}

impl ProcessWatch {
    pub fn open(pid: u32) -> io::Result<Self> {
        use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
        // SAFETY: plain open; owned below.
        let h = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        OwnedHandle::new(h)
            .map(|handle| Self { handle })
            .ok_or_else(io::Error::last_os_error)
    }

    /// Exit code if the process exits within `timeout`.
    pub fn wait(&self, timeout: Duration) -> io::Result<Option<u32>> {
        if !wait_handle(&self.handle, timeout)? {
            return Ok(None);
        }
        let mut code = 0u32;
        // SAFETY: valid process handle; `code` is an out-param.
        if unsafe { GetExitCodeProcess(self.handle.raw(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(code))
    }
}

/// Whether the current process is in any job.
pub fn current_process_in_job() -> io::Result<bool> {
    let mut result = 0;
    // SAFETY: pseudo-handle; null job means "any job".
    if unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut result) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(result != 0)
}

/// Breakaway rules of the job the current process is in: `None` outside any job,
/// `Some(true)` if children may break away (explicitly or silently).
pub fn current_job_allows_breakaway() -> io::Result<Option<bool>> {
    use windows_sys::Win32::System::JobObjects::{
        QueryInformationJobObject, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    };
    if !current_process_in_job()? {
        return Ok(None);
    }
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe_zeroed();
    // SAFETY: a null job handle means "the job of the calling process"; `info` is the
    // struct this information class expects, with its size.
    let ok = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            (&mut info as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = info.BasicLimitInformation.LimitFlags;
    Ok(Some(
        flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) != 0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_job_environment() {
        // Diagnostic: shows whether the test runner (e.g. cargo) put us in a job.
        eprintln!(
            "in_job={:?} allows_breakaway={:?}",
            current_process_in_job(),
            current_job_allows_breakaway()
        );
        assert!(current_job_allows_breakaway().is_ok());
    }

    #[test]
    fn quoting_follows_msvc_rules() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(
            quote_arg("C:\\Program Files\\x\\"),
            "\"C:\\Program Files\\x\\\\\""
        );
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote_arg("C:\\Проект"), "C:\\Проект");
    }

    #[test]
    fn env_block_is_sorted_and_double_terminated() {
        let block = env_block(&[("b".into(), "2".into()), ("A".into(), "1".into())]);
        let text = String::from_utf16_lossy(&block);
        assert_eq!(text, "A=1\0b=2\0\0");
    }

    #[test]
    fn spawn_passes_exact_environment_and_exit_code() {
        let cmd =
            std::env::var("ComSpec").unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".into());
        let p = spawn(&SpawnOptions {
            exe: cmd.into(),
            args: vec![
                "/d".into(),
                "/c".into(),
                "if defined AMIRI_X (exit 7) else (exit 3)".into(),
            ],
            env: Some(vec![
                ("AMIRI_X".into(), "1".into()),
                ("SystemRoot".into(), "C:\\Windows".into()),
            ]),
            flags: CREATE_NO_WINDOW,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(p.wait(Duration::from_secs(20)).unwrap(), Some(7));
    }
}
