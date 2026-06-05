#[derive(Debug, Clone)]
struct ProcessInfo {
    pid: u32,
    ppid: u32,
    name: String,
}

pub fn kill_acp_descendants(root_pid: Option<u32>) {
    let Some(root_pid) = root_pid else { return };
    platform::kill_acp_descendants(root_pid);
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

fn is_acp_wrapper_process(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "cmd.exe" | "node.exe" | "codex-acp.exe" | "node" | "codex-acp"
    )
}

#[cfg(windows)]
mod platform {
    use super::{collect_descendants, is_acp_wrapper_process, ProcessInfo};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    pub fn kill_acp_descendants(root_pid: u32) {
        let Ok(processes) = snapshot_processes() else {
            return;
        };

        let mut targets = collect_descendants(&processes, root_pid)
            .into_iter()
            .filter(|process| is_acp_wrapper_process(&process.name))
            .collect::<Vec<_>>();
        targets.sort_by_key(|process| std::cmp::Reverse(process.pid));

        for process in targets {
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

    fn kill_process(pid: u32) {
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() {
            return;
        }
        unsafe {
            let _ = TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

#[cfg(all(unix, target_os = "linux"))]
mod platform {
    use super::{collect_descendants, is_acp_wrapper_process, ProcessInfo};

    pub fn kill_acp_descendants(root_pid: u32) {
        let Ok(processes) = snapshot_processes() else {
            return;
        };

        let mut targets = collect_descendants(&processes, root_pid)
            .into_iter()
            .filter(|process| is_acp_wrapper_process(&process.name))
            .collect::<Vec<_>>();
        targets.sort_by_key(|process| std::cmp::Reverse(process.pid));

        for process in targets {
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

    fn kill_process(pid: u32) {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod platform {
    use super::{collect_descendants, is_acp_wrapper_process, ProcessInfo};

    pub fn kill_acp_descendants(root_pid: u32) {
        let Ok(output) = std::process::Command::new("ps")
            .args(["-axo", "pid=,ppid=,comm="])
            .output()
        else {
            return;
        };
        let processes = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_ps_line)
            .collect::<Vec<_>>();

        let mut targets = collect_descendants(&processes, root_pid)
            .into_iter()
            .filter(|process| is_acp_wrapper_process(&process.name))
            .collect::<Vec<_>>();
        targets.sort_by_key(|process| std::cmp::Reverse(process.pid));

        for process in targets {
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

    fn kill_process(pid: u32) {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    pub fn kill_acp_descendants(_root_pid: u32) {}
}
