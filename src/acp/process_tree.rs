#[derive(Debug, Clone)]
struct ProcessInfo {
    pid: u32,
    ppid: u32,
    name: String,
}

pub fn kill_acp_descendants(root_pid: Option<u32>) {
    let Some(root_pid) = root_pid else {
        tracing::debug!("kill_acp_descendants: no root pid, skipping");
        return;
    };
    tracing::debug!(root_pid, "kill_acp_descendants: walking process tree");
    platform::kill_acp_descendants(root_pid);
}

/// Best-effort TerminateProcess on a single PID. Used as a safety net for the
/// wrapper (root) process itself when the JobObject path can't be relied on
/// (e.g. AcpClient::Drop racing with task abortion). No-op when pid is None.
pub fn kill_process(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    tracing::debug!(pid, "kill_process: terminating");
    platform::kill_process(pid);
}

fn collect_descendants(processes: &[ProcessInfo], root_pid: u32) -> Vec<ProcessInfo> {
    let mut descendants = Vec::new();
    let mut stack = vec![root_pid];

    while let Some(parent) = stack.pop() {
        for process in processes.iter().filter(|process| process.ppid == parent) {
            descendants.push(process.clone());
            stack.push(process.pid);
        }
    }

    descendants
}

#[cfg(windows)]
mod platform {
    use super::{collect_descendants, ProcessInfo};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    pub fn kill_acp_descendants(root_pid: u32) {
        let processes = match snapshot_processes() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(root_pid, error = %e, "snapshot_processes failed");
                return;
            }
        };

        let mut targets = collect_descendants(&processes, root_pid);
        targets.sort_by_key(|process| std::cmp::Reverse(process.pid));

        if targets.is_empty() {
            tracing::debug!(root_pid, "no descendants found");
            return;
        }

        for process in targets {
            tracing::debug!(root_pid, pid = process.pid, name = %process.name, "terminating descendant");
            kill_process(process.pid);
        }
    }

    fn snapshot_processes() -> std::io::Result<Vec<ProcessInfo>> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }

        let mut processes = Vec::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while ok {
            processes.push(ProcessInfo {
                pid: entry.th32ProcessID,
                ppid: entry.th32ParentProcessID,
                name: wide_name_to_string(&entry.szExeFile),
            });
            ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }

        unsafe {
            CloseHandle(snapshot);
        }

        Ok(processes)
    }

    fn wide_name_to_string(name: &[u16]) -> String {
        let len = name.iter().position(|ch| *ch == 0).unwrap_or(name.len());
        String::from_utf16_lossy(&name[..len])
    }

    pub(super) fn kill_process(pid: u32) {
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() {
            tracing::debug!(
                pid,
                error = %std::io::Error::last_os_error(),
                "OpenProcess failed (likely already exited)"
            );
            return;
        }
        let terminated = unsafe { TerminateProcess(handle, 1) };
        if terminated == 0 {
            tracing::debug!(
                pid,
                error = %std::io::Error::last_os_error(),
                "TerminateProcess failed"
            );
        }
        unsafe {
            CloseHandle(handle);
        }
    }
}

#[cfg(all(unix, target_os = "linux"))]
mod platform {
    use super::{collect_descendants, ProcessInfo};

    pub fn kill_acp_descendants(root_pid: u32) {
        let processes = match snapshot_processes() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(root_pid, error = %e, "snapshot_processes failed");
                return;
            }
        };

        let mut targets = collect_descendants(&processes, root_pid);
        targets.sort_by_key(|process| std::cmp::Reverse(process.pid));

        if targets.is_empty() {
            tracing::debug!(root_pid, "no descendants found");
            return;
        }

        for process in targets {
            tracing::debug!(root_pid, pid = process.pid, name = %process.name, "terminating descendant");
            kill_process(process.pid);
        }
    }

    fn snapshot_processes() -> std::io::Result<Vec<ProcessInfo>> {
        let mut processes = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let Ok(entry) = entry else { continue };
            let file_name = entry.file_name();
            let Some(pid) = file_name.to_string_lossy().parse::<u32>().ok() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some(ppid) = parse_linux_ppid(&stat) else {
                continue;
            };
            let name = std::fs::read_to_string(entry.path().join("comm"))
                .unwrap_or_default()
                .trim()
                .to_string();
            processes.push(ProcessInfo { pid, ppid, name });
        }
        Ok(processes)
    }

    fn parse_linux_ppid(stat: &str) -> Option<u32> {
        let end = stat.rfind(") ")?;
        let tail = stat.get(end + 2..)?;
        let mut parts = tail.split_whitespace();
        let _state = parts.next()?;
        parts.next()?.parse().ok()
    }

    pub(super) fn kill_process(pid: u32) {
        if let Err(e) = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
        {
            tracing::debug!(pid, error = %e, "kill -TERM failed");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Err(e) = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
        {
            tracing::debug!(pid, error = %e, "kill -KILL failed");
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod platform {
    use super::{collect_descendants, ProcessInfo};

    pub fn kill_acp_descendants(root_pid: u32) {
        let output = match std::process::Command::new("ps")
            .args(["-axo", "pid=,ppid=,comm="])
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(root_pid, error = %e, "ps snapshot failed");
                return;
            }
        };
        let processes = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_ps_line)
            .collect::<Vec<_>>();

        let mut targets = collect_descendants(&processes, root_pid);
        targets.sort_by_key(|process| std::cmp::Reverse(process.pid));

        if targets.is_empty() {
            tracing::debug!(root_pid, "no descendants found");
            return;
        }

        for process in targets {
            tracing::debug!(root_pid, pid = process.pid, name = %process.name, "terminating descendant");
            kill_process(process.pid);
        }
    }

    fn parse_ps_line(line: &str) -> Option<ProcessInfo> {
        let mut parts = line.split_whitespace();
        let pid = parts.next()?.parse().ok()?;
        let ppid = parts.next()?.parse().ok()?;
        let name = std::path::Path::new(parts.next()?)
            .file_name()?
            .to_string_lossy()
            .to_string();
        Some(ProcessInfo { pid, ppid, name })
    }

    pub(super) fn kill_process(pid: u32) {
        if let Err(e) = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
        {
            tracing::debug!(pid, error = %e, "kill -TERM failed");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Err(e) = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
        {
            tracing::debug!(pid, error = %e, "kill -KILL failed");
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    pub fn kill_acp_descendants(_root_pid: u32) {}
    pub(super) fn kill_process(_pid: u32) {}
}
