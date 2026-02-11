//! Configuration module for ccgonext

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub agents: HashMap<String, AgentConfig>,
    pub timeouts: TimeoutConfig,
    pub web: WebConfig,
}

impl Default for Config {
    fn default() -> Self {
        let mut agents = HashMap::new();
        agents.insert("codex".to_string(), AgentConfig::codex_default());
        agents.insert("gemini".to_string(), AgentConfig::gemini_default());
        agents.insert("opencode".to_string(), AgentConfig::opencode_default());
        agents.insert("claudecode".to_string(), AgentConfig::claudecode_default());

        Self {
            server: ServerConfig::default(),
            agents,
            timeouts: TimeoutConfig::default(),
            web: WebConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub host: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 8765,
            host: "127.0.0.1".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub command: String,
    pub args: Vec<String>,
    pub log_provider: String,
    pub ready_pattern: String,
    pub error_patterns: Vec<String>,
    pub supports_cwd: bool,
    pub sentinel_template: String,
    pub sentinel_regex: String,
    pub done_template: String,
    pub done_regex: String,
    pub use_stability_heuristic: bool,
}

impl AgentConfig {
    /// Creates default config for Codex agent
    pub fn codex_default() -> Self {
        Self {
            command: "codex".to_string(),
            args: vec![],
            log_provider: "codex".to_string(),
            ready_pattern: r"^(>|codex>)".to_string(),
            error_patterns: vec!["Error:".to_string(), "Traceback".to_string()],
            supports_cwd: false,
            sentinel_template: "# MSG_ID:{id}\n{message}".to_string(),
            sentinel_regex: r"# MSG_ID:([a-f0-9-]+)".to_string(),
            done_template: "CCGO_DONE: {id}".to_string(),
            done_regex: r"(?mi)^\s*CCGO_DONE:\s*{id}\s*$".to_string(),
            use_stability_heuristic: false,
        }
    }

    /// Creates default config for Gemini agent
    pub fn gemini_default() -> Self {
        Self {
            command: "gemini".to_string(),
            args: vec![],
            log_provider: "gemini".to_string(),
            ready_pattern: r"(Gemini|>\s*$)".to_string(),
            error_patterns: vec!["Error:".to_string(), "Failed".to_string()],
            supports_cwd: false,
            sentinel_template: "[MSG_ID:{id}]\n{message}".to_string(),
            sentinel_regex: r"\[MSG_ID:([a-f0-9-]+)\]".to_string(),
            done_template: "CCGO_DONE: {id}".to_string(),
            done_regex: r"(?mi)^\s*CCGO_DONE:\s*{id}\s*$".to_string(),
            use_stability_heuristic: false,
        }
    }

    /// Creates default config for OpenCode agent
    pub fn opencode_default() -> Self {
        Self {
            command: "opencode".to_string(),
            args: vec![],
            log_provider: "opencode".to_string(),
            ready_pattern: r"(opencode|>\s*$)".to_string(),
            error_patterns: vec!["ERROR".to_string(), "Exception".to_string()],
            supports_cwd: false,
            sentinel_template: "[[MSG:{id}]]\n{message}".to_string(),
            sentinel_regex: r"\[\[MSG:([a-f0-9-]+)\]\]".to_string(),
            done_template: "CCGO_DONE: {id}".to_string(),
            done_regex: r"(?mi)^\s*CCGO_DONE:\s*{id}\s*$".to_string(),
            use_stability_heuristic: false,
        }
    }

    /// Creates default config for ClaudeCode agent
    pub fn claudecode_default() -> Self {
        Self {
            command: "claude".to_string(),
            args: vec![],
            log_provider: "pty".to_string(),
            ready_pattern: r"(?m)^>\s*$".to_string(),
            error_patterns: vec![
                r"(?i)^error:".to_string(),
                r"(?i)^failed".to_string(),
                r"(?i)^fatal".to_string(),
            ],
            supports_cwd: false,
            sentinel_template: "# CCGONEXT_MSG_ID:{id}\n{message}".to_string(),
            sentinel_regex: r"(?i)#\s*CCGONEXT_MSG_ID:\s*([0-9a-f-]{36})".to_string(),
            done_template: "CCGO_DONE: {id}".to_string(),
            done_regex: r"(?mi)^\s*CCGO_DONE:\s*{id}\s*$".to_string(),
            use_stability_heuristic: true,
        }
    }

    /// Overrides the command path. Empty or whitespace-only strings are ignored.
    pub fn with_command(mut self, command: String) -> Self {
        if !command.trim().is_empty() {
            self.command = command;
        }
        self
    }

    /// Replaces all args with the given list
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Appends additional args to existing args
    pub fn extend_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.args.extend(args);
        self
    }
}

#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    pub default: u64,
    pub startup: u64,
    pub ready_check: u64,
    pub queue_wait: u64,
    pub max_stuck_duration: u64,
    pub max_start_retries: u32,
    pub start_retry_delay_ms: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            default: 600,
            startup: 30,
            ready_check: 30,
            queue_wait: 60,
            max_stuck_duration: 300,
            max_start_retries: 3,
            start_retry_delay_ms: 1000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WebConfig {
    pub auth_token: Option<String>,
    pub input_enabled: bool,
    pub output_buffer_size: usize,
    pub project_root: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            auth_token: None,
            input_enabled: false,
            output_buffer_size: 10 * 1024 * 1024, // 10MB
            project_root: String::new(),
        }
    }
}

impl Config {
    pub fn get_agent(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();

        // Test agents
        assert!(config.agents.contains_key("codex"));
        assert!(config.agents.contains_key("gemini"));
        assert!(config.agents.contains_key("opencode"));
        assert!(config.agents.contains_key("claudecode"));

        // Test server config
        assert_eq!(config.server.port, 8765);
        assert_eq!(config.server.host, "127.0.0.1");

        // Test timeouts
        assert_eq!(config.timeouts.default, 600);
        assert_eq!(config.timeouts.startup, 30);

        // Test web config
        assert!(config.web.auth_token.is_none());
        assert!(!config.web.input_enabled);
    }

    #[test]
    fn test_config_get_agent() {
        let config = Config::default();

        let codex = config.get_agent("codex");
        assert!(codex.is_some());
        assert_eq!(codex.unwrap().command, "codex");

        let nonexistent = config.get_agent("nonexistent");
        assert!(nonexistent.is_none());
    }

    #[test]
    fn test_agent_config_claudecode() {
        let config = Config::default();
        let claudecode = config.get_agent("claudecode").unwrap();

        assert_eq!(claudecode.command, "claude");
        assert_eq!(claudecode.log_provider, "pty");
        assert!(claudecode.args.is_empty());
    }

    #[test]
    fn test_server_config_default() {
        let server = ServerConfig::default();
        assert_eq!(server.port, 8765);
        assert_eq!(server.host, "127.0.0.1");
    }

    #[test]
    fn test_timeout_config_default() {
        let timeouts = TimeoutConfig::default();
        assert_eq!(timeouts.default, 600);
        assert_eq!(timeouts.startup, 30);
        assert_eq!(timeouts.ready_check, 30);
        assert_eq!(timeouts.queue_wait, 60);
        assert_eq!(timeouts.max_stuck_duration, 300);
        assert_eq!(timeouts.max_start_retries, 3);
        assert_eq!(timeouts.start_retry_delay_ms, 1000);
    }

    #[test]
    fn test_web_config_default() {
        let web = WebConfig::default();
        assert!(web.auth_token.is_none());
        assert!(!web.input_enabled);
        assert_eq!(web.output_buffer_size, 10 * 1024 * 1024);
        assert!(web.project_root.is_empty());
    }

    #[test]
    fn test_config_clone() {
        let config1 = Config::default();
        let config2 = config1.clone();

        assert_eq!(config1.server.port, config2.server.port);
        assert_eq!(config1.agents.len(), config2.agents.len());
    }

    #[test]
    fn test_agent_config_with_command() {
        let config = AgentConfig::codex_default().with_command("my-codex".to_string());
        assert_eq!(config.command, "my-codex");
        // Other fields should remain default
        assert!(config.args.is_empty());
        assert_eq!(config.log_provider, "codex");
    }

    #[test]
    fn test_agent_config_with_command_empty_ignored() {
        let config = AgentConfig::codex_default().with_command(String::new());
        // Empty command should be ignored, keeping the default
        assert_eq!(config.command, "codex");
    }

    #[test]
    fn test_agent_config_with_command_whitespace_ignored() {
        let config = AgentConfig::codex_default().with_command("   ".to_string());
        // Whitespace-only command should be ignored, keeping the default
        assert_eq!(config.command, "codex");
    }

    #[test]
    fn test_agent_config_with_args() {
        let config = AgentConfig::gemini_default()
            .with_args(vec!["--yolo".to_string(), "--fast".to_string()]);
        assert_eq!(config.args, vec!["--yolo", "--fast"]);
        // Command should remain default
        assert_eq!(config.command, "gemini");
    }

    #[test]
    fn test_agent_config_extend_args() {
        let config = AgentConfig::codex_default()
            .with_args(vec!["-a".to_string(), "never".to_string()])
            .extend_args(vec!["--read-only".to_string()]);
        assert_eq!(config.args, vec!["-a", "never", "--read-only"]);
    }

    #[test]
    fn test_agent_config_builder_chain() {
        let config = AgentConfig::codex_default()
            .with_command("custom-codex".to_string())
            .with_args(vec!["-s".to_string(), "safe".to_string()]);

        assert_eq!(config.command, "custom-codex");
        assert_eq!(config.args, vec!["-s", "safe"]);
        // Other fields remain default
        assert_eq!(config.log_provider, "codex");
        assert_eq!(config.ready_pattern, r"^(>|codex>)");
    }

    #[test]
    fn test_all_agent_defaults_have_empty_args() {
        // Verify no dangerous hardcoded flags
        assert!(AgentConfig::codex_default().args.is_empty());
        assert!(AgentConfig::gemini_default().args.is_empty());
        assert!(AgentConfig::opencode_default().args.is_empty());
        assert!(AgentConfig::claudecode_default().args.is_empty());
    }

    #[test]
    fn test_config_default_contains_all_agents() {
        let config = Config::default();
        let expected_agents = ["codex", "gemini", "opencode", "claudecode"];

        for agent in &expected_agents {
            assert!(
                config.agents.contains_key(*agent),
                "Config::default() should contain agent '{}'",
                agent
            );
        }
        assert_eq!(config.agents.len(), expected_agents.len());
    }
}
