//! Agent adapter trait and implementations

use async_trait::async_trait;
use std::any::Any;
use std::path::Path;

mod claudecode;

pub use claudecode::ClaudeCodeAgent;

#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;

    fn get_ready_pattern(&self) -> &str;

    fn get_error_patterns(&self) -> &[String];

    fn get_startup_command(&self, working_dir: &Path) -> Vec<String>;

    fn inject_message_sentinel(&self, message: &str, message_id: &str) -> String;

    fn get_interrupt_sequence(&self) -> &[u8] {
        b"\x03" // Ctrl+C
    }

    fn should_auto_restart(&self, exit_code: i32) -> bool {
        exit_code != 0
    }

    fn extract_sentinel_id(&self, output: &str) -> Option<String>;

    fn get_done_regex(&self) -> &str;

    fn is_reply_complete(&self, text: &str, message_id: &str) -> bool;

    fn strip_done_marker(&self, text: &str, message_id: &str) -> String;

    fn use_stability_heuristic(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any;
}

pub struct GenericAgent {
    name: String,
    ready_pattern: String,
    error_patterns: Vec<String>,
    command: String,
    args: Vec<String>,
    supports_cwd: bool,
    sentinel_template: String,
    sentinel_regex: String,
    done_template: String,
    done_regex: String,
    use_stability_heuristic: bool,
}

impl GenericAgent {
    pub fn new(name: String, config: &crate::config::AgentConfig) -> Self {
        Self {
            name,
            ready_pattern: config.ready_pattern.clone(),
            error_patterns: config.error_patterns.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            supports_cwd: config.supports_cwd,
            sentinel_template: config.sentinel_template.clone(),
            sentinel_regex: config.sentinel_regex.clone(),
            done_template: config.done_template.clone(),
            done_regex: config.done_regex.clone(),
            use_stability_heuristic: config.use_stability_heuristic,
        }
    }
}

#[async_trait]
impl Agent for GenericAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn get_ready_pattern(&self) -> &str {
        &self.ready_pattern
    }

    fn get_error_patterns(&self) -> &[String] {
        &self.error_patterns
    }

    fn get_startup_command(&self, working_dir: &Path) -> Vec<String> {
        let mut cmd = vec![self.command.clone()];
        cmd.extend(self.args.clone());
        if self.supports_cwd && working_dir.exists() {
            cmd.push("--cwd".to_string());
            cmd.push(working_dir.display().to_string());
        }
        cmd
    }

    fn inject_message_sentinel(&self, message: &str, message_id: &str) -> String {
        let prefix = self
            .sentinel_template
            .replace("{id}", message_id)
            .replace("{message}", message);

        let done_marker = self.done_template.replace("{id}", message_id);

        format!(
            "{}\n\n\
            IMPORTANT:\n\
            - Reply normally, in English.\n\
            - End your reply with this exact final line (verbatim, on its own line):\n\
            {}",
            prefix, done_marker
        )
    }

    fn extract_sentinel_id(&self, output: &str) -> Option<String> {
        let pattern = regex::Regex::new(&self.sentinel_regex).ok()?;
        pattern.captures(output).map(|c| c[1].to_string())
    }

    fn get_done_regex(&self) -> &str {
        &self.done_regex
    }

    fn is_reply_complete(&self, text: &str, message_id: &str) -> bool {
        let pattern = self.done_regex.replace("{id}", &regex::escape(message_id));
        let re = match regex::Regex::new(&pattern) {
            Ok(r) => r,
            Err(_) => return false,
        };
        text.lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| re.is_match(l))
            .unwrap_or(false)
    }

    fn strip_done_marker(&self, text: &str, message_id: &str) -> String {
        let pattern = self.done_regex.replace("{id}", &regex::escape(message_id));
        let re = match regex::Regex::new(&pattern) {
            Ok(r) => r,
            Err(_) => return text.to_string(),
        };

        let lines: Vec<&str> = text.lines().collect();
        let mut result_lines: Vec<&str> = Vec::new();
        let mut found_marker = false;

        for line in lines.iter().rev() {
            if !found_marker && line.trim().is_empty() {
                continue;
            }
            if !found_marker && re.is_match(line) {
                found_marker = true;
                continue;
            }
            result_lines.push(line);
        }

        result_lines.reverse();
        result_lines.join("\n").trim_end().to_string()
    }

    fn use_stability_heuristic(&self) -> bool {
        self.use_stability_heuristic
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_agent(name: &str, config: &crate::config::AgentConfig) -> Box<dyn Agent> {
    // ClaudeCode is special because it uses PTY-based parsing instead of log files
    if name == "claudecode" {
        return Box::new(ClaudeCodeAgent::with_command(
            config.command.clone(),
            config.args.clone(),
        ));
    }

    // All other agents use GenericAgent with configuration
    Box::new(GenericAgent::new(name.to_string(), config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;

    fn create_test_config() -> AgentConfig {
        AgentConfig {
            command: "test".to_string(),
            args: vec![],
            log_provider: "test".to_string(),
            ready_pattern: r"^>".to_string(),
            error_patterns: vec!["Error:".to_string()],
            supports_cwd: false,
            sentinel_template: "# MSG_ID:{id}\n{message}".to_string(),
            sentinel_regex: r"# MSG_ID:([a-f0-9-]+)".to_string(),
            done_template: "CCGO_DONE: {id}".to_string(),
            done_regex: r"(?mi)^\s*CCGO_DONE:\s*{id}\s*$".to_string(),
            use_stability_heuristic: true,
        }
    }

    #[test]
    fn test_generic_agent_is_reply_complete() {
        let config = create_test_config();
        let agent = GenericAgent::new("test".to_string(), &config);
        let message_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        // Test with valid done marker
        let text = format!("Response content\nCCGO_DONE: {}", message_id);
        assert!(agent.is_reply_complete(&text, message_id));

        // Test with trailing blank lines
        let text_blanks = format!("Response\nCCGO_DONE: {}\n\n\n", message_id);
        assert!(agent.is_reply_complete(&text_blanks, message_id));

        // Test without marker
        assert!(!agent.is_reply_complete("No marker here", message_id));

        // Test with wrong ID - should NOT match
        let other_id = "00000000-0000-0000-0000-000000000000";
        let text_other = format!("Response\nCCGO_DONE: {}", other_id);
        assert!(!agent.is_reply_complete(&text_other, message_id));
    }

    #[test]
    fn test_generic_agent_strip_done_marker() {
        let config = create_test_config();
        let agent = GenericAgent::new("test".to_string(), &config);
        let message_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        // Test stripping marker
        let text = format!("Line 1\nLine 2\nCCGO_DONE: {}", message_id);
        let stripped = agent.strip_done_marker(&text, message_id);
        assert_eq!(stripped, "Line 1\nLine 2");

        // Test with trailing blank lines
        let text_blanks = format!("Content\nCCGO_DONE: {}\n\n", message_id);
        let stripped_blanks = agent.strip_done_marker(&text_blanks, message_id);
        assert_eq!(stripped_blanks, "Content");

        // Test without marker
        let no_marker = "Just content";
        assert_eq!(
            agent.strip_done_marker(no_marker, message_id),
            "Just content"
        );

        // Test with wrong ID - should NOT strip
        let other_id = "00000000-0000-0000-0000-000000000000";
        let text_other = format!("Content\nCCGO_DONE: {}", other_id);
        assert_eq!(agent.strip_done_marker(&text_other, message_id), text_other);
    }

    #[test]
    fn test_generic_agent_inject_sentinel_includes_done_instruction() {
        let config = create_test_config();
        let agent = GenericAgent::new("test".to_string(), &config);
        let message_id = "12345678-1234-1234-1234-123456789abc";

        let injected = agent.inject_message_sentinel("Hello", message_id);

        // Should contain sentinel
        assert!(injected.contains("MSG_ID:"));
        assert!(injected.contains(message_id));

        // Should contain done marker instruction
        assert!(injected.contains("CCGO_DONE:"));
        assert!(injected.contains("IMPORTANT:"));
        assert!(injected.contains("End your reply"));
    }

    #[test]
    fn test_generic_agent_get_done_regex() {
        let config = create_test_config();
        let agent = GenericAgent::new("test".to_string(), &config);

        assert_eq!(agent.get_done_regex(), r"(?mi)^\s*CCGO_DONE:\s*{id}\s*$");
    }
}
