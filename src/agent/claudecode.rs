//! ClaudeCode agent adapter
//!
//! ClaudeCode is unique: it does not produce separate log files,
//! so all response parsing is done via PTY output.
//!
//! ## Integration Requirements
//!
//! Unlike other agents (Codex/Gemini/OpenCode) that use LogProvider for response
//! detection, ClaudeCode requires direct PTY parsing. The `parse_pty_response`
//! method must be called by AgentSession after sending a message.
//!
//! ### Required Session Layer Changes
//!
//! 1. Detect if agent is ClaudeCode (check agent name or use a trait method)
//! 2. After writing message to PTY, instead of waiting for LogProvider:
//!    - Call `ClaudeCodeAgent::parse_pty_response(pty, start_offset, message_id)`
//!    - Handle the returned response or error
//! 3. Ensure timeout handling is consistent with other agents
//!
//! ### Example Integration (pseudo-code)
//!
//! ```rust,ignore
//! // In AgentSession::ask()
//! let start_offset = pty.get_current_offset().await;
//! pty.write_line(&message_with_sentinel).await?;
//!
//! if self.agent.name() == "claudecode" {
//!     // PTY-based parsing
//!     let claudecode = downcast_agent_to_claudecode(&self.agent);
//!     let response = claudecode.parse_pty_response(&pty, start_offset, &message_id).await?;
//!     return Ok(response);
//! } else {
//!     // Log-based parsing (existing logic)
//!     let entry = log_provider.get_latest_reply(baseline_offset).await;
//!     // ... existing logic
//! }
//! ```

use super::Agent;
use crate::pty::PtyHandle;
use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct ClaudeCodeAgent {
    command: String,
    args: Vec<String>,
    ready_pattern: String,
    ready_regex: Regex,
    error_patterns: Vec<String>,
    sentinel_regex: Regex,
    response_timeout: Duration,
    idle_timeout: Duration,
    ansi_regex: Regex,
}

impl ClaudeCodeAgent {
    pub fn new() -> Self {
        Self::with_command("claude".to_string(), vec![])
    }

    pub fn with_command(command: String, args: Vec<String>) -> Self {
        Self {
            command,
            args,
            ready_pattern: r"(?m)^>\s*$".to_string(),
            ready_regex: Regex::new(r"(?m)^>\s*$").expect("valid ready regex"),
            error_patterns: vec![
                r"(?i)^error:".to_string(),
                r"(?i)^failed".to_string(),
                r"(?i)^fatal".to_string(),
            ],
            sentinel_regex: Regex::new(r"(?i)#\s*CCGO_MSG_ID:\s*([0-9a-f-]{36})")
                .expect("valid sentinel regex"),
            response_timeout: Duration::from_secs(120),
            idle_timeout: Duration::from_secs(5),
            ansi_regex: Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").expect("valid ANSI regex"),
        }
    }

    /// Strip ANSI escape codes from text
    fn strip_ansi_codes(&self, text: &str) -> String {
        self.ansi_regex.replace_all(text, "").to_string()
    }

    /// Parse response from PTY output
    ///
    /// This is the core PTY parsing logic:
    /// 1. Read from start_offset incrementally
    /// 2. Strip ANSI codes
    /// 3. Detect sentinel (allows waiting across chunks)
    /// 4. Collect response until ready prompt reappears or timeout
    pub async fn parse_pty_response(
        &self,
        pty: &PtyHandle,
        start_offset: u64,
        expected_sentinel_id: &str,
    ) -> Result<String> {
        let deadline = Instant::now() + self.response_timeout;
        let mut saw_sentinel = false;
        let mut saw_output = false;
        let mut last_output_time = Instant::now();
        let mut response_lines = Vec::new();
        let mut last_read_offset = start_offset;
        let mut pending_line = String::new();

        loop {
            // Check absolute timeout
            if Instant::now() > deadline {
                if saw_sentinel {
                    // Got sentinel but incomplete response - return what we have
                    tracing::warn!("Response timeout, returning partial response");
                    return Ok(response_lines.join("\n"));
                } else {
                    anyhow::bail!("Timeout waiting for sentinel");
                }
            }

            // Get current offset
            let current_offset = pty.get_current_offset().await;
            if current_offset <= last_read_offset {
                // No new data yet
                if saw_output && Instant::now().duration_since(last_output_time) > self.idle_timeout
                {
                    // Idle timeout after seeing output
                    if saw_sentinel {
                        return Ok(response_lines.join("\n"));
                    } else {
                        anyhow::bail!("Idle timeout before seeing sentinel");
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            // Read new data from last_read_offset
            let data = match pty.read_from_offset(last_read_offset).await {
                Ok(d) => d,
                Err(e) => {
                    // Offset underflow - data was dropped
                    anyhow::bail!("PTY buffer offset underflow: {}", e);
                }
            };

            if data.is_empty() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            // Advance offset by bytes actually read
            last_read_offset = last_read_offset.saturating_add(data.len() as u64);

            // Convert to string and strip ANSI codes
            let text = String::from_utf8_lossy(&data);
            let stripped = self.strip_ansi_codes(&text);

            // Handle line buffering across chunks
            let mut combined = pending_line;
            combined.push_str(&stripped);
            let ends_with_newline = combined.ends_with('\n');
            let mut lines_iter = combined.split('\n').peekable();
            pending_line = String::new();

            while let Some(line) = lines_iter.next() {
                // Keep incomplete line for next chunk
                if !ends_with_newline && lines_iter.peek().is_none() {
                    pending_line = line.to_string();
                    break;
                }

                let line = line.trim_end_matches('\r');

                // Wait for sentinel across chunks
                if !saw_sentinel {
                    if let Some(found_id) = self.sentinel_regex.captures(line) {
                        let found_id = &found_id[1];
                        if found_id != expected_sentinel_id {
                            anyhow::bail!(
                                "Sentinel ID mismatch: expected {}, found {}",
                                expected_sentinel_id,
                                found_id
                            );
                        }
                        saw_sentinel = true;
                        // Don't set saw_output yet - wait for actual response content
                    }
                    continue;
                }

                // Check if ready prompt reappeared
                if self.ready_regex.is_match(line) {
                    return Ok(response_lines.join("\n"));
                }

                // Skip sentinel line in response
                if self.sentinel_regex.is_match(line) {
                    continue;
                }

                // Collect response line
                response_lines.push(line.to_string());
                saw_output = true;
                last_output_time = Instant::now();
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Default for ClaudeCodeAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for ClaudeCodeAgent {
    fn name(&self) -> &str {
        "claudecode"
    }

    fn get_ready_pattern(&self) -> &str {
        &self.ready_pattern
    }

    fn get_error_patterns(&self) -> &[String] {
        &self.error_patterns
    }

    fn get_startup_command(&self, _working_dir: &Path) -> Vec<String> {
        // Use simple command; working_dir is set via PTY spawn cwd
        // (portable_pty CommandBuilder::cwd is cross-platform and handles special chars)
        let mut cmd = vec![self.command.clone()];
        cmd.extend(self.args.clone());
        cmd
    }

    fn inject_message_sentinel(&self, message: &str, message_id: &str) -> String {
        // Use comment format to avoid ClaudeCode interpreting it
        // Format: # CCGO_MSG_ID:<uuid>\n<message>
        format!("# CCGO_MSG_ID:{}\n{}", message_id, message)
    }

    fn extract_sentinel_id(&self, output: &str) -> Option<String> {
        self.sentinel_regex
            .captures(output)
            .map(|c| c[1].to_string())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claudecode_agent_creation() {
        let agent = ClaudeCodeAgent::new();
        assert_eq!(agent.name(), "claudecode");
        assert!(!agent.get_error_patterns().is_empty());
    }

    #[test]
    fn test_claudecode_ready_pattern() {
        let agent = ClaudeCodeAgent::new();

        // Test matching ready prompt
        assert!(agent.ready_regex.is_match(">\n"));
        assert!(agent.ready_regex.is_match(">   \n"));
        assert!(!agent.ready_regex.is_match("prompt>"));
    }

    #[test]
    fn test_claudecode_sentinel_injection() {
        let agent = ClaudeCodeAgent::new();
        let message = "Hello, world!";
        let message_id = "12345678-1234-1234-1234-123456789abc";

        let injected = agent.inject_message_sentinel(message, message_id);
        assert!(injected.starts_with("# CCGO_MSG_ID:"));
        assert!(injected.contains(message_id));
        assert!(injected.contains(message));
    }

    #[test]
    fn test_claudecode_sentinel_extraction() {
        let agent = ClaudeCodeAgent::new();
        let message_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        let output = format!("# CCGO_MSG_ID: {}\nSome response", message_id);
        let extracted = agent.extract_sentinel_id(&output);
        assert_eq!(extracted, Some(message_id.to_string()));

        // Test case insensitive
        let output_upper = format!("# ccgo_msg_id: {}\nResponse", message_id);
        let extracted_upper = agent.extract_sentinel_id(&output_upper);
        assert_eq!(extracted_upper, Some(message_id.to_string()));

        // Test no match
        let no_match = agent.extract_sentinel_id("No sentinel here");
        assert_eq!(no_match, None);
    }

    #[test]
    fn test_claudecode_startup_command() {
        let agent = ClaudeCodeAgent::new();
        let cmd = agent.get_startup_command(Path::new("/tmp/test"));
        assert_eq!(cmd, vec!["claude".to_string()]);
    }

    #[test]
    fn test_claudecode_strip_ansi_codes() {
        let agent = ClaudeCodeAgent::new();

        // Test basic SGR codes
        let text_with_ansi = "\x1b[31mRed text\x1b[0m normal";
        let stripped = agent.strip_ansi_codes(text_with_ansi);
        assert_eq!(stripped, "Red text normal");

        // Test multiple ANSI codes
        let complex_ansi = "\x1b[1m\x1b[32mBold Green\x1b[0m\x1b[K";
        let stripped_complex = agent.strip_ansi_codes(complex_ansi);
        assert_eq!(stripped_complex, "Bold Green");

        // Test no ANSI codes
        let plain_text = "Plain text";
        let stripped_plain = agent.strip_ansi_codes(plain_text);
        assert_eq!(stripped_plain, "Plain text");
    }

    #[test]
    fn test_claudecode_default() {
        let agent1 = ClaudeCodeAgent::new();
        let agent2 = ClaudeCodeAgent::default();

        assert_eq!(agent1.name(), agent2.name());
        assert_eq!(agent1.get_ready_pattern(), agent2.get_ready_pattern());
    }
}
