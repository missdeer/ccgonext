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
        self.sentinel_template
            .replace("{id}", message_id)
            .replace("{message}", message)
    }

    fn extract_sentinel_id(&self, output: &str) -> Option<String> {
        let pattern = regex::Regex::new(&self.sentinel_regex).ok()?;
        pattern.captures(output).map(|c| c[1].to_string())
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
