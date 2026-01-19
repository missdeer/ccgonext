//! Gemini agent adapter

use super::Agent;
use async_trait::async_trait;
use std::path::Path;

pub struct GeminiAgent {
    ready_pattern: String,
    error_patterns: Vec<String>,
}

impl GeminiAgent {
    pub fn new() -> Self {
        Self {
            ready_pattern: r"(Gemini|>\s*$)".to_string(),
            error_patterns: vec!["Error:".to_string(), "Failed".to_string()],
        }
    }
}

impl Default for GeminiAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for GeminiAgent {
    fn name(&self) -> &str {
        "gemini"
    }

    fn get_ready_pattern(&self) -> &str {
        &self.ready_pattern
    }

    fn get_error_patterns(&self) -> &[String] {
        &self.error_patterns
    }

    fn get_startup_command(&self, working_dir: &Path) -> Vec<String> {
        let mut cmd = vec!["gemini".to_string()];
        if working_dir.exists() {
            cmd.push("--cwd".to_string());
            cmd.push(working_dir.display().to_string());
        }
        cmd
    }

    fn inject_message_sentinel(&self, message: &str, message_id: &str) -> String {
        // For Gemini, use invisible Unicode marker
        format!("\u{200B}MSG_ID:{}\u{200B}\n{}", message_id, message)
    }

    fn extract_sentinel_id(&self, output: &str) -> Option<String> {
        let pattern = regex::Regex::new(r"\u{200B}MSG_ID:([a-f0-9-]+)\u{200B}").ok()?;
        pattern.captures(output).map(|c| c[1].to_string())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
