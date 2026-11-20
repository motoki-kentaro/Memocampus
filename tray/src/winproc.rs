//! Starting `memocampus-mcp` from a GUI process on Windows, and keeping it from outliving the tray.
//!
//! One `CreateProcessW` call does everything the Issue #67 Slice 1 requirements ask for:
//!
//! - **Silent.** `DETACHED_PROCESS`: the child gets no console at all (not even a hidden one), so
//!   there is no window to show, now or at stop, restart or crash. Its stdin and stdout are `NUL`
//!   and its stderr is a pipe the tray reads.
//! - **Only the intended handles.** `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` names exactly three
//!   handles the child inherits: the read end of the management pipe, the write end of the stderr
//!   pipe and `NUL`. Every other inheritable handle in this process stays out of the child, and the
//!   tray's ends of both pipes are not inheritable at all.
//! - **No orphan, with no gap.** `PROC_THREAD_ATTRIBUTE_JOB_LIST` puts the child into an anonymous
//!   job object (`KILL_ON_JOB_CLOSE`) as part of its creation. There is no moment in which the
//!   child runs outside the job, so it needs no suspended start; if the tray dies, its handle to the
//!   job closes and Windows ends the child. A normal stop goes over the management pipe first.
//!
//! The job is anonymous (no name any other process could open) and holds only this child.

use crate::cmdline::command_line;
use crate::supervisor::{LaunchError, LaunchSpec, Launched, Launcher, ManagedChild};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{
    GetLastError, SetHandleInformation, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ,
    GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
    EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// The command-line flag `memocampus-mcp` reads the management handle from.
const MANAGEMENT_FLAG: &str = "--management-pipe-handle";
/// The one byte the server takes as a stop request (`mcp-server/src/mgmt.rs`).
const STOP_BYTE: u8 = b'S';
const MAX_LINE: u64 = 4096;

/// Handles are inheritable only while a child is being created; one creation at a time keeps a
/// second thread's `CreateProcess` from inheriting them in that moment.
static SPAWN: Mutex<()> = Mutex::new(());

pub struct WinLauncher {
    pub exe: PathBuf,
    /// Set (or, with `None`, removed) in the child's environment, over the tray's own. Production
    /// leaves this empty: the child sees the tray's environment, so both resolve the same
    /// application directory.
    pub env: Vec<(OsString, Option<OsString>)>,
    /// Appended after the settings' arguments. Production leaves this empty; the tests use it to
    /// select what a stand-in server does.
    pub extra_args: Vec<OsString>,
}

impl WinLauncher {
    pub fn new(exe: PathBuf) -> Self {
        Self {
            exe,
            env: Vec::new(),
            extra_args: Vec::new(),
        }
    }
}

impl Launcher for WinLauncher {
    fn launch(&mut self, spec: &LaunchSpec) -> Result<Launched, LaunchError> {
        if !self.exe.is_file() {
            return Err(LaunchError::ExeMissing);
        }
        let mut args = spec.args.clone();
        args.extend(self.extra_args.iter().cloned());
        spawn_managed(&self.exe, &args, &self.env)
    }
}

struct WinChild {
    // Field order is drop order: the process handle first, then the job. Closing the job's last
    // handle ends the child if it is still running (the orphan guard).
    process: OwnedHandle,
    _job: OwnedHandle,
    pid: u32,
    management: Option<File>,
}

impl ManagedChild for WinChild {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn try_exit(&mut self) -> io::Result<Option<i32>> {
        let handle = self.process.as_raw_handle() as HANDLE;
        // SAFETY: a valid process handle owned by `self`.
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_OBJECT_0 => {
                let mut code = 0u32;
                // SAFETY: as above; `code` is live for the call.
                if unsafe { GetExitCodeProcess(handle, &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(code as i32))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    fn request_stop(&mut self) -> io::Result<()> {
        let pipe = self
            .management
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        pipe.write_all(&[STOP_BYTE])?;
        pipe.flush()
    }

    fn terminate(&mut self) -> io::Result<()> {
        // The handle this tray created: never a process found by name or by id.
        // SAFETY: a valid process handle owned by `self`.
        if unsafe { TerminateProcess(self.process.as_raw_handle() as HANDLE, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

fn inheritable() -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    }
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle just returned by the OS and owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// A pipe with the child's end inheritable and the tray's end not: `(read, write)`.
fn pipe(child_reads: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let attributes = inheritable();
    let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: two live out-pointers and a live attribute block.
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let (read, write) = (owned(read)?, owned(write)?);
    let ours = if child_reads { &write } else { &read };
    // SAFETY: a valid handle owned above.
    if unsafe { SetHandleInformation(ours.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, 0) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((read, write))
}

fn job() -> io::Result<OwnedHandle> {
    // SAFETY: no attributes and no name: an anonymous job nothing else can open.
    let job = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
    // SAFETY: a zeroed plain-data struct, then one flag set.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: a valid job handle and a live struct of the size passed.
    let set = unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if set == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// The environment block of the child: the tray's, with `changes` applied (names compare
/// case-insensitively, as Windows does). `None` when nothing changes: the child then inherits as is.
fn environment(changes: &[(OsString, Option<OsString>)]) -> Option<Vec<u16>> {
    if changes.is_empty() {
        return None;
    }
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (name, value) in changes {
        let same = |other: &OsString| {
            other.to_string_lossy().to_lowercase() == name.to_string_lossy().to_lowercase()
        };
        vars.retain(|(existing, _)| !same(existing));
        if let Some(value) = value {
            vars.push((name.clone(), value.clone()));
        }
    }
    vars.sort_by_key(|(name, _)| name.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (name, value) in vars {
        block.extend(name.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Some(block)
}

fn spawn_managed(
    exe: &std::path::Path,
    args: &[OsString],
    env: &[(OsString, Option<OsString>)],
) -> Result<Launched, LaunchError> {
    let _one_at_a_time = SPAWN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let failed = |_: io::Error| LaunchError::Failed;

    let job = job().map_err(failed)?;
    let (management_read, management_write) = pipe(true).map_err(failed)?;
    let (stderr_read, stderr_write) = pipe(false).map_err(failed)?;
    let nul_name = wide(OsStr::new("NUL"));
    let attributes = inheritable();
    // SAFETY: a NUL-terminated name and a live attribute block.
    let nul = owned(unsafe {
        CreateFileW(
            nul_name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    })
    .map_err(failed)?;

    // What the child may inherit, and nothing else.
    let inherited: [HANDLE; 3] = [
        management_read.as_raw_handle() as HANDLE,
        stderr_write.as_raw_handle() as HANDLE,
        nul.as_raw_handle() as HANDLE,
    ];
    let job_list: [HANDLE; 1] = [job.as_raw_handle() as HANDLE];

    let mut all_args: Vec<OsString> = args.to_vec();
    all_args.push(MANAGEMENT_FLAG.into());
    all_args.push(
        (management_read.as_raw_handle() as usize)
            .to_string()
            .into(),
    );
    let mut line = command_line(exe.as_os_str(), &all_args);
    let application = wide(exe.as_os_str());
    let environment = environment(env);

    // The attribute list: its size first, then the list, in memory aligned for pointers.
    let mut size = 0usize;
    // SAFETY: the documented size query: a null list and a live out-size.
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &mut size) };
    let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
    let list = storage.as_mut_ptr().cast();
    // SAFETY: `list` has `size` bytes, as the query above asked.
    if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut size) } == 0 {
        return Err(LaunchError::Failed);
    }
    struct ListGuard(*mut std::ffi::c_void);
    impl Drop for ListGuard {
        fn drop(&mut self) {
            // SAFETY: an initialised list that is deleted once.
            unsafe { DeleteProcThreadAttributeList(self.0) };
        }
    }
    let _guard = ListGuard(list);
    // SAFETY: the value arrays outlive `CreateProcessW` below; sizes are their byte lengths.
    let attributes_set = unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            inherited.as_ptr().cast(),
            std::mem::size_of_val(&inherited),
            std::ptr::null_mut(),
            std::ptr::null(),
        ) != 0
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                job_list.as_ptr().cast(),
                std::mem::size_of_val(&job_list),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) != 0
    };
    if !attributes_set {
        return Err(LaunchError::Failed);
    }

    // SAFETY: plain-data structs, zeroed, then the fields that matter set.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = nul.as_raw_handle() as HANDLE;
    startup.StartupInfo.hStdOutput = nul.as_raw_handle() as HANDLE;
    startup.StartupInfo.hStdError = stderr_write.as_raw_handle() as HANDLE;
    startup.lpAttributeList = list;
    // SAFETY: as above.
    let mut created: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let mut flags = EXTENDED_STARTUPINFO_PRESENT | DETACHED_PROCESS;
    if environment.is_some() {
        flags |= CREATE_UNICODE_ENVIRONMENT;
    }
    // SAFETY: every pointer is to live, NUL-terminated or sized data; `line` is writable as
    // `CreateProcessW` requires; the handle list names handles that are open and inheritable.
    let ok = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            flags,
            environment
                .as_ref()
                .map_or(std::ptr::null(), |block| block.as_ptr().cast()),
            std::ptr::null(),
            &startup.StartupInfo,
            &mut created,
        )
    };
    if ok == 0 {
        // SAFETY: straight after the call.
        let error = unsafe { GetLastError() };
        return Err(
            if error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND {
                LaunchError::ExeMissing
            } else {
                LaunchError::Failed
            },
        );
    }
    let process = owned(created.hProcess).map_err(failed)?;
    drop(owned(created.hThread));

    // The child holds its ends now. Ours of them are closed so that the stderr pipe ends when the
    // child does, and so that no copy of the child's ends stays in the tray.
    drop(management_read);
    drop(stderr_write);
    drop(nul);

    let (send, lines) = mpsc::channel();
    let stderr = File::from(stderr_read);
    std::thread::Builder::new()
        .name("mcp-stderr".into())
        .spawn(move || {
            let mut reader = io::BufReader::new(stderr);
            let mut bytes = Vec::new();
            loop {
                bytes.clear();
                match reader.by_ref().take(MAX_LINE).read_until(b'\n', &mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if send.send(text).is_err() {
                    break;
                }
            }
        })
        .map_err(failed)?;

    Ok(Launched {
        child: Box::new(WinChild {
            process,
            _job: job,
            pid: created.dwProcessId,
            management: Some(File::from(management_write)),
        }),
        lines,
    })
}
