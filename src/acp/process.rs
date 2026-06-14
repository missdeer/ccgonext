use process_wrap::tokio::TokioChildWrapper;
use std::collections::HashMap;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};

#[cfg(not(windows))]
use process_wrap::tokio::{KillOnDrop, ProcessGroup, TokioCommandWrap};
#[cfg(not(windows))]
use std::process::Stdio;
#[cfg(not(windows))]
use tokio::process::Command;

pub struct AcpProcess {
    pub child: Box<dyn TokioChildWrapper>,
    pub root_pid: Option<u32>,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    pub stderr: ChildStderr,
}

impl AcpProcess {
    pub fn spawn(
        command: &str,
        args: &[String],
        env_vars: &HashMap<String, String>,
        cwd: &Path,
    ) -> anyhow::Result<Self> {
        // Windows: use our hand-rolled JobObject path. process-wrap's `JobObject`
        // wrapper associates a completion port with the job; that association
        // empirically prevents `KILL_ON_JOB_CLOSE` from firing when ccgo dies,
        // leaving the codex-acp subtree alive. Confirmed via tasklist after
        // matching reproductions on both paths.
        #[cfg(windows)]
        {
            super::windows_job::spawn(command, args, env_vars, cwd)
        }

        #[cfg(not(windows))]
        {
            let mut cmd = Command::new(command);
            cmd.args(args);
            cmd.current_dir(cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (k, v) in env_vars {
                cmd.env(k, v);
            }

            // ProcessGroup::leader() makes the child the head of a new pgrp so
            // that `start_kill` (= killpg via process-wrap's ProcessGroupChild)
            // wipes the whole subtree — agents like codex-acp spawn their own
            // children, and `KillOnDrop` alone (tokio kill_on_drop = TerminateProcess
            // on the direct child only) would orphan them on Linux/macOS.
            let mut wrap = TokioCommandWrap::from(cmd);
            wrap.wrap(ProcessGroup::leader());
            wrap.wrap(KillOnDrop);

            let mut child = wrap
                .spawn()
                .map_err(|e| anyhow::anyhow!("Failed to spawn ACP agent '{}': {}", command, e))?;
            let root_pid = child.id();

            let stdin = child
                .stdin()
                .take()
                .ok_or_else(|| anyhow::anyhow!("stdin not piped"))?;
            let stdout = child
                .stdout()
                .take()
                .ok_or_else(|| anyhow::anyhow!("stdout not piped"))?;
            let stderr = child
                .stderr()
                .take()
                .ok_or_else(|| anyhow::anyhow!("stderr not piped"))?;

            Ok(Self {
                child,
                root_pid,
                stdin,
                stdout: BufReader::new(stdout),
                stderr,
            })
        }
    }

    pub async fn kill(&mut self) {
        let _ = Box::into_pin(self.child.kill()).await;
    }
}

pub async fn drain_stderr(stderr: ChildStderr, agent_name: String) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end();
                if !trimmed.is_empty() {
                    tracing::warn!(agent = %agent_name, "stderr: {}", trimmed);
                }
            }
            Err(e) => {
                tracing::debug!(agent = %agent_name, "stderr read error: {}", e);
                break;
            }
        }
    }
}
