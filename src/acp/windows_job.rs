//! Hand-rolled Windows JobObject spawn path.
//!
//! Replaces process-wrap's `JobObject` wrapper on Windows. process-wrap also
//! associates an IoCompletionPort with the job (`JobObjectAssociateCompletion
//! PortInformation`); empirically that association keeps the codex-acp tree
//! alive even after ccgo dies, so `KILL_ON_JOB_CLOSE` never fires. We create
//! a plain job with only KILL_ON_JOB_CLOSE, AssignProcessToJobObject, and
//! resume — verified to terminate the whole tree.
//!
//! Setting `CCGONEXT_WIN_JOB_DEBUG=1` adds verbose per-spawn diagnostics:
//! T0..T4 markers, periodic `JobObjectBasicProcessIdList` dumps, and member
//! snapshots around start_kill/Drop.
#![cfg(windows)]

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::mem::{size_of, size_of_val, zeroed};
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::ptr::{null, null_mut};
use std::time::Duration;

use process_wrap::tokio::TokioChildWrapper;
use tokio::io::BufReader;
use tokio::process::{Child, Command};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};

/// windows-sys 0.61 uses raw i32 for Win32 Bool. Aliased here for readability.
type Bool = i32;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

use super::process::AcpProcess;

pub const VERBOSE_ENV_FLAG: &str = "CCGONEXT_WIN_JOB_DEBUG";

fn verbose() -> bool {
    std::env::var(VERBOSE_ENV_FLAG)
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(false)
}

/// Send-safe wrapper around a Win32 job handle. Stored as `usize` so the
/// async block's state machine never holds a raw `*mut c_void` across await
/// points (rustc rejects that even with `unsafe impl Send`).
#[derive(Copy, Clone)]
struct JobHandle(usize);
unsafe impl Send for JobHandle {}
unsafe impl Sync for JobHandle {}

impl JobHandle {
    fn from_raw(h: HANDLE) -> Self {
        Self(h as usize)
    }
    fn raw(self) -> HANDLE {
        self.0 as HANDLE
    }
}

/// RAII guard that owns a raw job HANDLE during the spawn setup window.
/// Closes the handle on Drop unless `defuse()` is called first. Used so that
/// any `?` / `bail!` between `create_job()` and the final `JobOwnedChild`
/// construction can't leak the job.
struct JobHandleGuard(Option<HANDLE>);

impl JobHandleGuard {
    fn new(h: HANDLE) -> Self {
        Self(Some(h))
    }
    fn raw(&self) -> HANDLE {
        self.0.expect("JobHandleGuard already defused")
    }
    fn defuse(mut self) -> HANDLE {
        self.0.take().expect("JobHandleGuard already defused")
    }
}

impl Drop for JobHandleGuard {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            unsafe { CloseHandle(h) };
        }
    }
}

pub fn spawn(
    command: &str,
    args: &[String],
    env_vars: &HashMap<String, String>,
    cwd: &Path,
) -> anyhow::Result<AcpProcess> {
    let verbose = verbose();
    if verbose {
        log_self_job_status();
    }

    let mut cmd = Command::new("cmd.exe");
    cmd.arg("/C")
        .arg(command)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_SUSPENDED)
        .kill_on_drop(true);
    for (k, v) in env_vars {
        cmd.env(k, v);
    }

    // Guard the job HANDLE from `create_job()` onward so any early-return path
    // (spawn failure, take() returning None, Assign/resume failure) frees it.
    // Final ownership transfers into JobOwnedChild via `defuse()`.
    let job_guard = JobHandleGuard::new(create_job()?);
    let job = job_guard.raw();
    tracing::debug!(
        job = format!("{:p}", job),
        agent = %command,
        "windows_job: created JobObject (KILL_ON_JOB_CLOSE)"
    );
    if verbose {
        log_extended_limits(job, "T1: our JobObject limits");
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn cmd.exe: {}", e))?;
    let root_pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("Child PID missing"))?;
    let child_handle = child
        .raw_handle()
        .ok_or_else(|| anyhow::anyhow!("Child handle missing"))? as HANDLE;
    if verbose {
        tracing::info!(
            pid = root_pid,
            handle = format!("{:p}", child_handle),
            "T2: cmd.exe spawned (suspended)"
        );
        log_in_any_job(child_handle, "T2: cmd.exe before Assign — in ANY job?");
    }

    let assigned = unsafe { AssignProcessToJobObject(job, child_handle) };
    if assigned == 0 {
        let err = unsafe { GetLastError() };
        let _ = child.start_kill();
        anyhow::bail!("AssignProcessToJobObject failed (Win32 error {})", err);
    }
    if verbose {
        tracing::info!(returned = assigned, "T3: AssignProcessToJobObject");
        log_in_specific_job(child_handle, job, "T3: cmd.exe in OUR job?");
        log_job_members(job, "T3: our job members right after Assign");
    }

    if let Err(e) = resume_process_threads(root_pid) {
        let _ = child.start_kill();
        anyhow::bail!("resume_process_threads failed: {}", e);
    }
    if verbose {
        tracing::info!(pid = root_pid, "T4: resumed cmd.exe main thread(s)");
    }

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdin not piped"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdout not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("stderr not piped"))?;

    // Spawn the diagnostic snapshot task AFTER stdio extraction. Spawning it
    // earlier risks a detached task — if any `take()` fails and we bail, the
    // local `JoinHandle` goes out of scope without abort (dropping a JoinHandle
    // detaches the task rather than cancelling it), so the task would outlive
    // JobHandleGuard's CloseHandle and read a stale HANDLE.
    let diagnostic_task = if verbose {
        let job_for_task = JobHandle::from_raw(job);
        Some(tokio::spawn(async move {
            for delay_ms in [400_u64, 1_200, 3_000, 6_000, 12_000, 30_000] {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                log_job_members(job_for_task.raw(), &format!("T4+{}ms", delay_ms));
            }
        }))
    } else {
        None
    };

    // Defuse the guard now that JobOwnedChild will own the handle.
    let wrapper: Box<dyn TokioChildWrapper> = Box::new(JobOwnedChild {
        inner: Some(child),
        job: Some(JobHandle::from_raw(job_guard.defuse())),
        root_pid,
        exit_status: None,
        verbose,
        diagnostic_task,
    });

    Ok(AcpProcess {
        child: wrapper,
        root_pid: Some(root_pid),
        stdin,
        stdout: BufReader::new(stdout),
        stderr,
    })
}

fn create_job() -> anyhow::Result<HANDLE> {
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        anyhow::bail!("CreateJobObjectW failed: {}", io::Error::last_os_error());
    }
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            size_of_val(&info) as u32,
        )
    };
    if ok == 0 {
        let err = io::Error::last_os_error();
        unsafe { CloseHandle(job) };
        anyhow::bail!("SetInformationJobObject(KILL_ON_JOB_CLOSE) failed: {}", err);
    }
    Ok(job)
}

fn log_self_job_status() {
    let me = unsafe { GetCurrentProcess() };
    let mut in_job: Bool = 0;
    let ok = unsafe { IsProcessInJob(me, null_mut(), &mut in_job) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        tracing::warn!(error = %err, "T0: IsProcessInJob(self) failed");
        return;
    }
    tracing::info!(in_job = in_job != 0, "T0: ccgo itself in some job?");

    if in_job == 0 {
        return;
    }

    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    let mut returned: u32 = 0;
    let ok = unsafe {
        QueryInformationJobObject(
            null_mut(),
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut _,
            size_of_val(&info) as u32,
            &mut returned,
        )
    };
    if ok == 0 {
        let err = io::Error::last_os_error();
        tracing::warn!(error = %err, "T0: QueryInformationJobObject(self) failed");
        return;
    }
    log_limit_flags(
        info.BasicLimitInformation.LimitFlags,
        "T0: ccgo's outer job",
    );
}

fn log_extended_limits(job: HANDLE, label: &str) {
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    let mut returned: u32 = 0;
    let ok = unsafe {
        QueryInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut _,
            size_of_val(&info) as u32,
            &mut returned,
        )
    };
    if ok == 0 {
        let err = io::Error::last_os_error();
        tracing::warn!(error = %err, "{}: query failed", label);
        return;
    }
    log_limit_flags(info.BasicLimitInformation.LimitFlags, label);
}

fn log_limit_flags(flags: u32, label: &str) {
    tracing::info!(
        flags = format!("0x{:08x}", flags),
        kill_on_close = flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0,
        breakaway_ok = flags & JOB_OBJECT_LIMIT_BREAKAWAY_OK != 0,
        silent_breakaway_ok = flags & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK != 0,
        "{}",
        label
    );
}

fn log_in_any_job(process_handle: HANDLE, label: &str) {
    let mut b: Bool = 0;
    let ok = unsafe { IsProcessInJob(process_handle, null_mut(), &mut b) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        tracing::warn!(error = %err, "{}: IsProcessInJob(any) failed", label);
        return;
    }
    tracing::info!(in_any_job = b != 0, "{}", label);
}

fn log_in_specific_job(process_handle: HANDLE, job: HANDLE, label: &str) {
    let mut b: Bool = 0;
    let ok = unsafe { IsProcessInJob(process_handle, job, &mut b) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        tracing::warn!(error = %err, "{}: IsProcessInJob(specific) failed", label);
        return;
    }
    tracing::info!(in_our_job = b != 0, "{}", label);
}

fn log_job_members(job: HANDLE, label: &str) {
    const SLOTS: usize = 128;
    // Allocate as `Vec<u64>` so the buffer is 8-byte aligned on both 32- and
    // 64-bit Windows. A `Vec<u8>` would only guarantee 1-byte alignment — UB
    // under Rust's strict-provenance rules. A `Vec<usize>` works on 64-bit
    // (where it's 8 bytes) but under-allocates the 8-byte header on 32-bit
    // (where usize is 4). Compute the size in bytes from the struct layout
    // (header + flexible-array slack for SLOTS extra PIDs), then round up to
    // u64 units.
    let bytes = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() + (SLOTS - 1) * size_of::<usize>();
    let units = bytes.div_ceil(size_of::<u64>());
    let mut buf: Vec<u64> = vec![0; units];
    let cap_bytes = (units * size_of::<u64>()) as u32;
    let mut returned: u32 = 0;
    let ok = unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicProcessIdList,
            buf.as_mut_ptr() as *mut _,
            cap_bytes,
            &mut returned,
        )
    };
    if ok == 0 {
        let err = io::Error::last_os_error();
        tracing::warn!(error = %err, label, "QueryInformationJobObject(BasicProcessIdList) failed");
        return;
    }
    let list = unsafe { &*(buf.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };
    let assigned = list.NumberOfAssignedProcesses;
    // Cap NumberOfProcessIdsInList against our buffer to avoid OOB reads if
    // the kernel reports more than we allocated (shouldn't happen, but the
    // field is untrusted input from a syscall).
    let in_list = (list.NumberOfProcessIdsInList as usize).min(SLOTS);
    let pids = unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), in_list) };
    let members: Vec<String> = pids
        .iter()
        .map(|&p| {
            let pid = p as u32;
            let name = lookup_process_name(pid).unwrap_or_else(|| "?".to_string());
            format!("{}({})", pid, name)
        })
        .collect();
    tracing::info!(
        assigned,
        in_list,
        members = ?members,
        "{}",
        label
    );
}

fn lookup_process_name(pid: u32) -> Option<String> {
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut ok = unsafe { Process32FirstW(snap, &mut entry) };
    let mut found = None;
    while ok != 0 {
        if entry.th32ProcessID == pid {
            let len = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            found = Some(String::from_utf16_lossy(&entry.szExeFile[..len]));
            break;
        }
        ok = unsafe { Process32NextW(snap, &mut entry) };
    }
    unsafe { CloseHandle(snap) };
    found
}

fn resume_process_threads(pid: u32) -> io::Result<()> {
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry: THREADENTRY32 = unsafe { zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut ok = unsafe { Thread32First(snap, &mut entry) };
    let mut resumed = 0u32;
    while ok != 0 {
        if entry.th32OwnerProcessID == pid {
            let th = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !th.is_null() {
                let prev = unsafe { ResumeThread(th) };
                if prev != u32::MAX {
                    resumed += 1;
                }
                unsafe { CloseHandle(th) };
            }
        }
        ok = unsafe { Thread32Next(snap, &mut entry) };
    }
    unsafe { CloseHandle(snap) };
    if resumed == 0 {
        return Err(io::Error::other(format!(
            "no threads resumed for pid {}",
            pid
        )));
    }
    Ok(())
}

struct JobOwnedChild {
    inner: Option<Child>,
    /// `Some` while we own the job; `None` after `into_inner` or Drop has
    /// released ownership. Wrapping in Option keeps `into_inner` contract-safe:
    /// the returned Child must not be killed by a lingering KILL_ON_JOB_CLOSE.
    job: Option<JobHandle>,
    root_pid: u32,
    exit_status: Option<ExitStatus>,
    verbose: bool,
    /// `tokio::JoinHandle` of the verbose-mode periodic snapshot task. Aborted
    /// in Drop so it can't read the job HANDLE after CloseHandle reused it.
    diagnostic_task: Option<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for JobOwnedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JobOwnedChild(pid={})", self.root_pid)
    }
}

impl TokioChildWrapper for JobOwnedChild {
    fn inner(&self) -> &Child {
        self.inner
            .as_ref()
            .expect("JobOwnedChild::inner called after into_inner")
    }

    fn inner_mut(&mut self) -> &mut Child {
        self.inner
            .as_mut()
            .expect("JobOwnedChild::inner_mut called after into_inner")
    }

    fn into_inner(mut self: Box<Self>) -> Child {
        // Release the job WITHOUT closing the handle: closing would trigger
        // KILL_ON_JOB_CLOSE and instantly terminate the Child the caller just
        // took ownership of. We intentionally leak the HANDLE — caller has
        // chosen to opt out of our job-lifetime management.
        if let Some(h) = self.job.take() {
            tracing::debug!(
                root_pid = self.root_pid,
                job = format!("{:p}", h.raw()),
                "windows_job: into_inner — releasing job HANDLE without closing"
            );
        }
        if let Some(task) = self.diagnostic_task.take() {
            task.abort();
        }
        self.inner
            .take()
            .expect("JobOwnedChild::into_inner called twice")
    }

    fn start_kill(&mut self) -> io::Result<()> {
        let job = match self.job {
            Some(h) => h.raw(),
            None => return Ok(()),
        };
        tracing::debug!(
            root_pid = self.root_pid,
            job = format!("{:p}", job),
            "windows_job: start_kill — TerminateJobObject"
        );
        if self.verbose {
            log_job_members(job, "before TerminateJobObject");
        }
        let ok = unsafe { TerminateJobObject(job, 1) };
        if ok == 0 {
            let err = io::Error::last_os_error();
            tracing::warn!(error = %err, "TerminateJobObject failed");
            return Err(err);
        }
        Ok(())
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let inner = match self.inner.as_mut() {
            Some(c) => c,
            None => return Ok(self.exit_status),
        };
        match inner.try_wait()? {
            Some(s) => {
                self.exit_status = Some(s);
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    fn wait(&mut self) -> Box<dyn Future<Output = io::Result<ExitStatus>> + '_> {
        Box::new(async move {
            if let Some(status) = self.exit_status {
                return Ok(status);
            }
            let inner = self
                .inner
                .as_mut()
                .ok_or_else(|| io::Error::other("child taken"))?;
            let status = inner.wait().await?;
            self.exit_status = Some(status);
            Ok(status)
        })
    }
}

impl Drop for JobOwnedChild {
    fn drop(&mut self) {
        // Abort the diagnostic task first so it can't poll the HANDLE after we
        // close it (Win32 would return ERROR_INVALID_HANDLE in the best case,
        // but the HANDLE could be reused for an unrelated object).
        if let Some(task) = self.diagnostic_task.take() {
            task.abort();
        }
        if let Some(h) = self.job.take() {
            let job = h.raw();
            tracing::debug!(
                root_pid = self.root_pid,
                job = format!("{:p}", job),
                "windows_job: closing job handle (triggers KILL_ON_JOB_CLOSE)"
            );
            if self.verbose {
                log_job_members(job, "before CloseHandle(job)");
            }
            unsafe { CloseHandle(job) };
        }
    }
}
