//! Agent adapter trait and implementations

use async_trait::async_trait;
use std::any::Any;
use std::path::Path;

mod claudecode;
mod codex;
mod gemini;
mod opencode;

pub use claudecode::ClaudeCodeAgent;
pub use codex::CodexAgent;
pub use gemini::GeminiAgent;
pub use opencode::OpenCodeAgent;

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
}

impl GenericAgent {
    pub fn new(
        name: String,
        command: String,
        args: Vec<String>,
        ready_pattern: String,
        error_patterns: Vec<String>,
    ) -> Self {
        Self {
            name,
            ready_pattern,
            error_patterns,
            command,
            args,
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
        if working_dir.exists() {
            // Some agents support --cwd or similar
        }
        cmd
    }

    fn inject_message_sentinel(&self, message: &str, message_id: &str) -> String {
        format!("<<MSG_ID:{}>> {}", message_id, message)
    }

    fn extract_sentinel_id(&self, output: &str) -> Option<String> {
        let pattern = regex::Regex::new(r"<<MSG_ID:([a-f0-9-]+)>>").ok()?;
        pattern.captures(output).map(|c| c[1].to_string())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_agent(name: &str, config: &crate::config::AgentConfig) -> Box<dyn Agent> {
    match name {
        "codex" => Box::new(CodexAgent::new()),
        "gemini" => Box::new(GeminiAgent::new()),
        "opencode" => Box::new(OpenCodeAgent::new()),
        "claudecode" => Box::new(ClaudeCodeAgent::new()),
        _ => Box::new(GenericAgent::new(
            name.to_string(),
            config.command.clone(),
            config.args.clone(),
            String::new(),
            vec![],
        )),
    }
}
