use process_wrap::tokio::{KillOnDrop, TokioChildWrapper, TokioCommandWrap};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};

#[cfg(windows)]
use process_wrap::tokio::JobObject;

pub struct AcpProcess {
    pub child: Box<dyn TokioChildWrapper>,
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
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd.exe");
            c.arg("/C").arg(command).args(args);
            c
        } else {
            let mut c = Command::new(command);
            c.args(args);
            c
        };

        cmd.current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (k, v) in env_vars {
            cmd.env(k, v);
        }

        let mut wrap = TokioCommandWrap::from(cmd);
        #[cfg(windows)]
        wrap.wrap(JobObject);
        wrap.wrap(KillOnDrop);

        let mut child = wrap
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn ACP agent '{}': {}", command, e))?;

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
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
        })
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
