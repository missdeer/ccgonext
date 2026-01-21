//! PTY management layer

use anyhow::Result;
use bytes::BytesMut;
use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

/// Handle terminal query sequences by sending appropriate responses.
/// This is needed because macOS native PTY doesn't automatically respond to
/// terminal queries like Windows ConPTY does.
fn handle_terminal_queries(data: &[u8], writer: &mut dyn std::io::Write) -> std::io::Result<()> {
    // Search for terminal query sequences in the raw bytes
    // We need to respond immediately to avoid timeouts

    // Check for Cursor Position Report (CPR) query: ESC[6n
    if data.windows(4).any(|w| w == b"\x1b[6n") {
        // Respond with cursor at position 1,1: ESC[1;1R
        writer.write_all(b"\x1b[1;1R")?;
        writer.flush()?;
        tracing::debug!("[PTY] Responded to CPR query with position 1;1");
    }

    // Check for Device Status Report (DSR) query: ESC[5n
    if data.windows(4).any(|w| w == b"\x1b[5n") {
        // Respond with "terminal is OK": ESC[0n
        writer.write_all(b"\x1b[0n")?;
        writer.flush()?;
        tracing::debug!("[PTY] Responded to DSR query with OK status");
    }

    // Check for Primary Device Attributes (DA) query: ESC[c
    if data.windows(3).any(|w| w == b"\x1b[c") {
        // Respond as VT100: ESC[?1;0c
        writer.write_all(b"\x1b[?1;0c")?;
        writer.flush()?;
        tracing::debug!("[PTY] Responded to DA query as VT100");
    }

    Ok(())
}

/// Adapt command for Windows execution.
/// On Windows, Node.js CLI tools (codex, gemini, etc.) are installed as .cmd batch files.
/// CreateProcessW cannot directly execute .cmd/.ps1 files, so we wrap them appropriately:
/// - .ps1 files are executed with powershell.exe
/// - .cmd/.bat and bare commands are wrapped with cmd.exe /c
///
/// Security note: We use `-ExecutionPolicy Bypass` for PowerShell scripts. This is a
/// deliberate trade-off for developer convenience in an automation tool context.
/// The alternative (`RemoteSigned`) would require users to configure their system
/// or sign scripts, which creates friction for local development workflows.
/// This does NOT elevate privileges - it only affects script execution policy.
#[cfg(windows)]
fn adapt_command_for_windows(command: &[String]) -> Vec<String> {
    if command.is_empty() {
        return command.to_vec();
    }

    let program = &command[0];
    let lower = program.to_lowercase();

    // .exe files can be executed directly by CreateProcessW
    if lower.ends_with(".exe") {
        return command.to_vec();
    }

    // .ps1 files need PowerShell
    // -NoProfile: Don't load user profile (faster, avoids side effects)
    // -NonInteractive: Don't prompt for input (required for automation)
    // -ExecutionPolicy Bypass: Allow script execution (see security note above)
    // -File: Execute the script file
    if lower.ends_with(".ps1") {
        let mut adapted = vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-File".to_string(),
        ];
        adapted.extend(command.iter().cloned());
        return adapted;
    }

    // Everything else (bare commands, .cmd, .bat, .com) needs cmd.exe /c
    // This lets Windows resolve the command via PATH and PATHEXT
    let mut adapted = vec!["cmd.exe".to_string(), "/c".to_string()];
    adapted.extend(command.iter().cloned());
    adapted
}

#[cfg(not(windows))]
fn adapt_command_for_windows(command: &[String]) -> Vec<String> {
    command.to_vec()
}

/// PTY buffer with absolute offset tracking
/// This ensures offset-based response correlation remains correct under buffer trimming
pub struct PtyBuffer {
    buffer: BytesMut,
    base_offset: u64,
    write_offset: u64,
    limit: usize,
}

impl PtyBuffer {
    pub fn new(limit: usize) -> Self {
        Self {
            buffer: BytesMut::with_capacity(limit),
            base_offset: 0,
            write_offset: 0,
            limit,
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        if data.len() >= self.limit {
            self.buffer.clear();
            let start = data.len() - self.limit;
            self.buffer.extend_from_slice(&data[start..]);
            self.base_offset = self.write_offset + start as u64;
            self.write_offset += data.len() as u64;
        } else {
            let new_total = self.buffer.len() + data.len();
            if new_total > self.limit {
                let to_remove = new_total - self.limit;
                let _ = self.buffer.split_to(to_remove);
                self.base_offset += to_remove as u64;
            }
            self.buffer.extend_from_slice(data);
            self.write_offset += data.len() as u64;
        }
    }

    pub fn read_from_offset(&self, offset: u64) -> Result<&[u8]> {
        if offset < self.base_offset {
            anyhow::bail!(
                "Offset {} < base_offset {}, data dropped",
                offset,
                self.base_offset
            );
        }
        let rel_offset = (offset - self.base_offset) as usize;
        if rel_offset > self.buffer.len() {
            anyhow::bail!(
                "Offset {} beyond write_offset {}",
                offset,
                self.write_offset
            );
        }
        Ok(&self.buffer[rel_offset..])
    }

    pub fn current_offset(&self) -> u64 {
        self.write_offset
    }

    pub fn base_offset(&self) -> u64 {
        self.base_offset
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buffer
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

pub struct PtyHandle {
    write_tx: mpsc::Sender<PtyCommand>,
    output_tx: broadcast::Sender<Vec<u8>>,
    buffer: Arc<Mutex<PtyBuffer>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    shutdown: Arc<AtomicBool>,
}

enum PtyCommand {
    Write {
        data: Vec<u8>,
        response: oneshot::Sender<Result<()>>,
    },
    Resize {
        cols: u16,
        rows: u16,
        response: oneshot::Sender<Result<()>>,
    },
    Shutdown,
}

impl PtyHandle {
    pub fn spawn_command(
        command: &[String],
        working_dir: &Path,
        buffer_limit: usize,
    ) -> Result<Self> {
        if command.is_empty() {
            anyhow::bail!("Empty command");
        }

        // Adapt command for Windows (.cmd/.bat script support)
        let command = adapt_command_for_windows(command);

        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(&command[0]);
        for arg in &command[1..] {
            cmd.arg(arg);
        }

        // Inherit environment variables from current process
        for (key, value) in std::env::vars() {
            cmd.env(key, value);
        }

        if working_dir.exists() {
            cmd.cwd(working_dir);
        }

        // Spawn command and save child handle
        let child = pair.slave.spawn_command(cmd)?;
        let child: Arc<Mutex<Box<dyn Child + Send + Sync>>> = Arc::new(Mutex::new(child));

        // Get reader BEFORE moving master
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        // Wrap writer in Arc<Mutex> so both reader and writer threads can access it
        let writer = Arc::new(std::sync::Mutex::new(writer));
        let writer_for_queries = writer.clone();

        let (output_tx, _) = broadcast::channel(1024);
        let buffer = Arc::new(Mutex::new(PtyBuffer::new(buffer_limit)));

        // Channel for write commands
        let (write_tx, mut write_rx) = mpsc::channel::<PtyCommand>(256);

        // Master handle for resize - move after getting reader
        let master = pair.master;

        // Shutdown flag
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_writer = shutdown.clone();
        let shutdown_reader = shutdown.clone();

        // Spawn write handler thread
        let writer_for_commands = writer.clone();
        std::thread::spawn(move || {
            while let Some(cmd) = write_rx.blocking_recv() {
                match cmd {
                    PtyCommand::Write { data, response } => {
                        let result = {
                            let mut w = writer_for_commands.lock().unwrap();
                            w.write_all(&data)
                                .and_then(|_| w.flush())
                                .map_err(|e| anyhow::anyhow!("{}", e))
                        };
                        let _ = response.send(result);
                    }
                    PtyCommand::Resize {
                        cols,
                        rows,
                        response,
                    } => {
                        let result = master
                            .resize(PtySize {
                                rows,
                                cols,
                                pixel_width: 0,
                                pixel_height: 0,
                            })
                            .map_err(|e| anyhow::anyhow!("{}", e));
                        let _ = response.send(result);
                    }
                    PtyCommand::Shutdown => {
                        shutdown_writer.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });

        // Spawn output reader thread with terminal query response handler
        let output_tx_clone = output_tx.clone();
        let buffer_clone = buffer.clone();

        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                if shutdown_reader.load(Ordering::SeqCst) {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let data = buf[..n].to_vec();

                        // Check for terminal query sequences and respond automatically
                        // This must happen BEFORE broadcasting to avoid race conditions
                        {
                            let mut w = writer_for_queries.lock().unwrap();
                            let _ = handle_terminal_queries(&data, &mut *w);
                        }

                        // Broadcast to WebSocket subscribers
                        let _ = output_tx_clone.send(data.clone());

                        // Always update offsets, even if buffer_limit is 0
                        let mut buf_lock = buffer_clone.blocking_lock();
                        buf_lock.write(&data);
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            write_tx,
            output_tx,
            buffer,
            child,
            shutdown,
        })
    }

    pub async fn write(&self, data: &[u8]) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.write_tx
            .send(PtyCommand::Write {
                data: data.to_vec(),
                response: tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("PTY channel closed"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Response channel closed"))?
    }

    /// Send text followed by Enter key to the PTY.
    ///
    /// Uses a two-step approach similar to terminal multiplexers (tmux/WezTerm):
    /// 1. Send the text content
    /// 2. Brief delay to let TUI process the input
    /// 3. Send Enter key (CR byte)
    ///
    /// This ensures proper handling by TUI applications running in raw mode.
    pub async fn write_line(&self, line: &str) -> Result<()> {
        tracing::debug!(
            "[PTY] write_line called with {} bytes, has_newlines={}",
            line.len(),
            line.contains('\n')
        );
        let has_newlines = line.contains('\n');

        if has_newlines {
            // Multi-line: use bracketed paste to prevent line-by-line execution
            tracing::debug!("[PTY] Using bracketed paste mode");
            // Send paste start sequence
            self.write(b"\x1b[200~").await?;

            // Send content
            self.write(line.as_bytes()).await?;

            // Send paste end sequence
            self.write(b"\x1b[201~").await?;

            // Brief delay to let TUI process bracketed paste content
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        } else {
            // Single-line: send directly
            tracing::debug!("[PTY] Sending single-line: {:?}", line);
            self.write(line.as_bytes()).await?;

            // Brief delay to let TUI process input
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Send Enter key (CR byte triggers input submission in most TUIs)
        tracing::debug!("[PTY] Sending Enter key");
        self.write(b"\r").await
    }

    pub fn subscribe_output(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    pub async fn get_buffer(&self) -> Vec<u8> {
        self.buffer.lock().await.as_slice().to_vec()
    }

    pub async fn get_current_offset(&self) -> u64 {
        self.buffer.lock().await.current_offset()
    }

    pub async fn read_from_offset(&self, offset: u64) -> Result<Vec<u8>> {
        let buf = self.buffer.lock().await;
        buf.read_from_offset(offset).map(|s| s.to_vec())
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.write_tx
            .send(PtyCommand::Resize {
                cols,
                rows,
                response: tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("PTY channel closed"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Response channel closed"))?
    }

    /// Kill the child process
    pub async fn kill(&self) -> Result<()> {
        let mut child = self.child.lock().await;
        child.kill().map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Wait for child process to exit
    pub async fn wait(&self) -> Result<portable_pty::ExitStatus> {
        let mut child = self.child.lock().await;
        child.wait().map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Check if child process is still running
    pub async fn try_wait(&self) -> Result<Option<portable_pty::ExitStatus>> {
        let mut child = self.child.lock().await;
        child.try_wait().map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Wait for process exit with timeout.
    /// Uses polling since portable-pty's Child::wait() is synchronous and would
    /// block the async runtime. The 100ms polling interval is a reasonable tradeoff
    /// between responsiveness and CPU overhead (10 syscalls/sec).
    async fn wait_for_exit(
        &self,
        timeout: std::time::Duration,
    ) -> Option<portable_pty::ExitStatus> {
        let result = tokio::time::timeout(timeout, async {
            loop {
                match self.try_wait().await {
                    Ok(Some(status)) => return Some(status),
                    Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
                    Err(e) => {
                        tracing::warn!("Error checking process status: {}", e);
                        return None;
                    }
                }
            }
        })
        .await;

        result.ok().flatten()
    }

    /// Graceful shutdown: send /quit command, wait for agent to exit, then force kill if needed
    pub async fn shutdown(&self) {
        // Step 1: Send /quit command with timeout to prevent blocking on stuck PTY I/O
        tracing::info!("Sending /quit command to agent...");
        let quit_result =
            tokio::time::timeout(std::time::Duration::from_secs(2), self.write_line("/quit")).await;

        match quit_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("Failed to send /quit command: {}", e),
            Err(_) => tracing::warn!("/quit command timed out, PTY may be blocked"),
        }

        // Step 2: Wait up to 5 seconds for process to exit gracefully
        if let Some(status) = self.wait_for_exit(std::time::Duration::from_secs(5)).await {
            tracing::info!("Agent exited gracefully with status: {:?}", status);
        } else {
            // Step 3: Force kill if still running
            tracing::warn!("Agent did not exit after /quit, force killing...");

            if let Err(e) = self.kill().await {
                tracing::warn!("Failed to kill child process: {}", e);
            }

            // Wait briefly for process to exit after kill
            if self
                .wait_for_exit(std::time::Duration::from_millis(500))
                .await
                .is_none()
            {
                tracing::error!("Child process did not exit even after force kill");
            }
        }

        // Signal shutdown to worker threads AFTER process termination.
        // This ordering ensures I/O loops remain active to capture any final
        // output from the dying process before we tear down the channels.
        self.shutdown.store(true, Ordering::SeqCst);

        // Use timeout to prevent blocking if channel is full or writer is stuck
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.write_tx.send(PtyCommand::Shutdown),
        )
        .await;
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        // Set shutdown flag
        self.shutdown.store(true, Ordering::SeqCst);

        // Try to kill child process synchronously
        // Note: We can't use async here, so we use try_lock
        if let Ok(mut child) = self.child.try_lock() {
            if let Err(e) = child.kill() {
                tracing::debug!("Failed to kill child on drop: {}", e);
            }
        }
    }
}

pub struct PtyManager {
    handles: Arc<Mutex<std::collections::HashMap<String, Arc<PtyHandle>>>>,
    buffer_limit: usize,
}

impl PtyManager {
    pub fn new(buffer_limit: usize) -> Self {
        Self {
            handles: Arc::new(Mutex::new(std::collections::HashMap::new())),
            buffer_limit,
        }
    }

    pub async fn create(
        &self,
        agent_name: &str,
        command: &[String],
        working_dir: &Path,
    ) -> Result<Arc<PtyHandle>> {
        let handle = Arc::new(PtyHandle::spawn_command(
            command,
            working_dir,
            self.buffer_limit,
        )?);
        self.handles
            .lock()
            .await
            .insert(agent_name.to_string(), handle.clone());
        Ok(handle)
    }

    pub async fn get(&self, agent_name: &str) -> Option<Arc<PtyHandle>> {
        self.handles.lock().await.get(agent_name).cloned()
    }

    pub async fn remove(&self, agent_name: &str) -> Option<Arc<PtyHandle>> {
        self.handles.lock().await.remove(agent_name)
    }

    pub async fn list(&self) -> Vec<String> {
        self.handles.lock().await.keys().cloned().collect()
    }

    pub fn buffer_limit(&self) -> usize {
        self.buffer_limit
    }

    /// Shutdown all PTY handles
    pub async fn shutdown_all(&self) {
        let handles: Vec<_> = self.handles.lock().await.drain().collect();
        for (name, handle) in handles {
            tracing::info!("Shutting down PTY for agent: {}", name);
            handle.shutdown().await;
        }
    }
}

impl Drop for PtyManager {
    fn drop(&mut self) {
        // Try to kill all child processes synchronously
        if let Ok(handles) = self.handles.try_lock() {
            for (name, handle) in handles.iter() {
                if let Ok(mut child) = handle.child.try_lock() {
                    if let Err(e) = child.kill() {
                        tracing::debug!("Failed to kill {} on manager drop: {}", name, e);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_buffer_basic_write_read() {
        let mut buf = PtyBuffer::new(100);
        assert_eq!(buf.current_offset(), 0);
        assert_eq!(buf.base_offset(), 0);
        assert!(buf.is_empty());

        buf.write(b"hello");
        assert_eq!(buf.current_offset(), 5);
        assert_eq!(buf.base_offset(), 0);
        assert_eq!(buf.len(), 5);

        let data = buf.read_from_offset(0).unwrap();
        assert_eq!(data, b"hello");

        let data = buf.read_from_offset(2).unwrap();
        assert_eq!(data, b"llo");
    }

    #[test]
    fn test_pty_buffer_offset_tracking() {
        let mut buf = PtyBuffer::new(10);

        buf.write(b"12345");
        assert_eq!(buf.current_offset(), 5);
        assert_eq!(buf.base_offset(), 0);

        buf.write(b"67890");
        assert_eq!(buf.current_offset(), 10);
        assert_eq!(buf.base_offset(), 0);
        assert_eq!(buf.len(), 10);

        // Next write should trigger trimming
        buf.write(b"ABC");
        assert_eq!(buf.current_offset(), 13);
        assert_eq!(buf.base_offset(), 3); // First 3 bytes dropped
        assert_eq!(buf.len(), 10);
        assert_eq!(buf.as_slice(), b"4567890ABC");
    }

    #[test]
    fn test_pty_buffer_large_write() {
        let mut buf = PtyBuffer::new(10);

        // Write data larger than limit
        buf.write(b"0123456789ABCDEFGHIJ");
        assert_eq!(buf.current_offset(), 20);
        assert_eq!(buf.base_offset(), 10); // First 10 bytes dropped
        assert_eq!(buf.len(), 10);
        assert_eq!(buf.as_slice(), b"ABCDEFGHIJ");
    }

    #[test]
    fn test_pty_buffer_read_from_offset_errors() {
        let mut buf = PtyBuffer::new(10);
        buf.write(b"12345");

        // Try to read from future offset
        let result = buf.read_from_offset(10);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("beyond write_offset"));

        // Trigger trimming
        buf.write(b"67890ABCDEF");
        assert_eq!(buf.base_offset(), 6);

        // Try to read from dropped offset
        let result = buf.read_from_offset(0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("data dropped"));

        // Reading from valid offset should work
        let data = buf.read_from_offset(6).unwrap();
        assert_eq!(data, b"7890ABCDEF");
    }

    #[test]
    fn test_pty_buffer_incremental_reads() {
        let mut buf = PtyBuffer::new(50);

        buf.write(b"First chunk. ");
        let offset1 = buf.current_offset();

        buf.write(b"Second chunk. ");
        let offset2 = buf.current_offset();

        buf.write(b"Third chunk.");

        // Read from different offsets
        assert_eq!(
            buf.read_from_offset(0).unwrap(),
            b"First chunk. Second chunk. Third chunk."
        );
        assert_eq!(
            buf.read_from_offset(offset1).unwrap(),
            b"Second chunk. Third chunk."
        );
        assert_eq!(buf.read_from_offset(offset2).unwrap(), b"Third chunk.");
    }

    #[test]
    fn test_pty_buffer_zero_limit() {
        let mut buf = PtyBuffer::new(0);

        buf.write(b"test");
        assert_eq!(buf.current_offset(), 4);
        assert_eq!(buf.base_offset(), 4);
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn test_pty_manager_create_and_get() {
        let manager = PtyManager::new(1024);

        // List should be empty initially
        assert!(manager.list().await.is_empty());

        // Create PTY (using 'echo' command which exists on all platforms)
        let result = manager
            .create(
                "test",
                &["echo".to_string(), "hello".to_string()],
                Path::new("."),
            )
            .await;

        if result.is_ok() {
            // Get the handle
            let handle = manager.get("test").await;
            assert!(handle.is_some());

            // List should contain the agent
            let list = manager.list().await;
            assert_eq!(list.len(), 1);
            assert_eq!(list[0], "test");

            // Remove the handle
            let removed = manager.remove("test").await;
            assert!(removed.is_some());
            assert!(manager.list().await.is_empty());
        }
    }

    #[tokio::test]
    async fn test_pty_manager_buffer_limit() {
        let manager = PtyManager::new(512);
        assert_eq!(manager.buffer_limit(), 512);
    }

    #[test]
    fn test_adapt_command_for_windows() {
        // Empty command
        let empty: Vec<String> = vec![];
        assert_eq!(adapt_command_for_windows(&empty), empty);

        // Command with .exe extension should pass through
        let exe_cmd = vec!["node.exe".to_string(), "--version".to_string()];
        assert_eq!(adapt_command_for_windows(&exe_cmd), exe_cmd);

        // Command with absolute path ending in .exe should pass through
        let abs_path = vec!["C:\\Program Files\\node.exe".to_string()];
        assert_eq!(adapt_command_for_windows(&abs_path), abs_path);

        // .cmd/.bat files should be wrapped with cmd.exe /c on Windows
        let cmd_cmd = vec!["codex.cmd".to_string(), "exec".to_string()];
        let adapted_cmd = adapt_command_for_windows(&cmd_cmd);
        #[cfg(windows)]
        {
            assert_eq!(adapted_cmd.len(), 4);
            assert_eq!(adapted_cmd[0], "cmd.exe");
            assert_eq!(adapted_cmd[1], "/c");
            assert_eq!(adapted_cmd[2], "codex.cmd");
            assert_eq!(adapted_cmd[3], "exec");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(adapted_cmd, cmd_cmd);
        }

        // .ps1 files should be wrapped with powershell.exe on Windows
        let ps1_cmd = vec![
            "script.ps1".to_string(),
            "-Param".to_string(),
            "value".to_string(),
        ];
        let adapted_ps1 = adapt_command_for_windows(&ps1_cmd);
        #[cfg(windows)]
        {
            assert_eq!(adapted_ps1.len(), 9);
            assert_eq!(adapted_ps1[0], "powershell.exe");
            assert_eq!(adapted_ps1[1], "-NoProfile");
            assert_eq!(adapted_ps1[2], "-NonInteractive");
            assert_eq!(adapted_ps1[3], "-ExecutionPolicy");
            assert_eq!(adapted_ps1[4], "Bypass");
            assert_eq!(adapted_ps1[5], "-File");
            assert_eq!(adapted_ps1[6], "script.ps1");
            assert_eq!(adapted_ps1[7], "-Param");
            assert_eq!(adapted_ps1[8], "value");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(adapted_ps1, ps1_cmd);
        }

        // Plain command should be wrapped on Windows, passed through on other platforms
        let plain_cmd = vec!["codex".to_string(), "--help".to_string()];
        let adapted = adapt_command_for_windows(&plain_cmd);
        #[cfg(windows)]
        {
            assert_eq!(adapted.len(), 4);
            assert_eq!(adapted[0], "cmd.exe");
            assert_eq!(adapted[1], "/c");
            assert_eq!(adapted[2], "codex");
            assert_eq!(adapted[3], "--help");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(adapted, plain_cmd);
        }
    }
}
