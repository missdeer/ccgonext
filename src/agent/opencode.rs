//! OpenCode agent adapter

use super::Agent;
use async_trait::async_trait;
use std::path::Path;

pub struct OpenCodeAgent {
    ready_pattern: String,
    error_patterns: Vec<String>,
}

impl OpenCodeAgent {
    pub fn new() -> Self {
        Self {
            ready_pattern: r"(opencode|>\s*$)".to_string(),
            error_patterns: vec!["ERROR".to_string(), "Exception".to_string()],
        }
    }
}

impl Default for OpenCodeAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for OpenCodeAgent {
    fn name(&self) -> &str {
        "opencode"
    }

    fn get_ready_pattern(&self) -> &str {
        &self.ready_pattern
    }

    fn get_error_patterns(&self) -> &[String] {
        &self.error_patterns
    }

    fn get_startup_command(&self, working_dir: &Path) -> Vec<String> {
        let mut cmd = vec!["opencode".to_string()];
        if working_dir.exists() {
            cmd.push("--cwd".to_string());
            cmd.push(working_dir.display().to_string());
        }
        cmd
    }

    fn inject_message_sentinel(&self, message: &str, message_id: &str) -> String {
        // For OpenCode, use a special prefix
        format!("[[MSG:{}]]\n{}", message_id, message)
    }

    fn extract_sentinel_id(&self, output: &str) -> Option<String> {
        let pattern = regex::Regex::new(r"\[\[MSG:([a-f0-9-]+)\]\]").ok()?;
        pattern.captures(output).map(|c| c[1].to_string())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
