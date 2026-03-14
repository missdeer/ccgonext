use crate::acp::callbacks::CallbackPolicy;
use std::collections::HashMap;
use std::time::Duration;

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
    pub acp_command: String,
    pub acp_args: Vec<String>,
    pub env_vars: HashMap<String, String>,
    pub callback_policy: CallbackPolicy,
    pub idle_timeout: Duration,
}

impl AgentConfig {
    pub fn codex_default() -> Self {
        Self {
            acp_command: "codex-acp".to_string(),
            acp_args: vec![],
            env_vars: HashMap::new(),
            callback_policy: CallbackPolicy::AutoApprove,
            idle_timeout: Duration::from_secs(900),
        }
    }

    pub fn gemini_default() -> Self {
        Self {
            acp_command: "gemini".to_string(),
            acp_args: vec!["--acp".to_string()],
            env_vars: HashMap::new(),
            callback_policy: CallbackPolicy::AutoApprove,
            idle_timeout: Duration::from_secs(900),
        }
    }

    pub fn opencode_default() -> Self {
        Self {
            acp_command: "opencode".to_string(),
            acp_args: vec!["acp".to_string()],
            env_vars: HashMap::new(),
            callback_policy: CallbackPolicy::AutoApprove,
            idle_timeout: Duration::from_secs(900),
        }
    }

    pub fn claudecode_default() -> Self {
        Self {
            acp_command: "claude-agent-acp".to_string(),
            acp_args: vec![],
            env_vars: HashMap::new(),
            callback_policy: CallbackPolicy::AutoApprove,
            idle_timeout: Duration::from_secs(900),
        }
    }

    pub fn with_command(mut self, command: String) -> Self {
        if !command.trim().is_empty() {
            self.acp_command = command;
        }
        self
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.acp_args = args;
        self
    }

    pub fn extend_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.acp_args.extend(args);
        self
    }

    pub fn with_callback_policy(mut self, callback_policy: CallbackPolicy) -> Self {
        self.callback_policy = callback_policy;
        self
    }

    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }
}

#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    pub default: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self { default: 600 }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WebConfig {
    pub auth_token: Option<String>,
    pub project_root: String,
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
        assert!(config.agents.contains_key("codex"));
        assert!(config.agents.contains_key("gemini"));
        assert!(config.agents.contains_key("opencode"));
        assert!(config.agents.contains_key("claudecode"));
        assert_eq!(config.server.port, 8765);
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.timeouts.default, 600);
        assert!(config.web.auth_token.is_none());
    }

    #[test]
    fn test_config_get_agent() {
        let config = Config::default();
        let codex = config.get_agent("codex");
        assert!(codex.is_some());
        assert_eq!(codex.unwrap().acp_command, "codex-acp");
        assert!(config.get_agent("nonexistent").is_none());
    }

    #[test]
    fn test_agent_config_defaults() {
        let codex = AgentConfig::codex_default();
        assert_eq!(codex.acp_command, "codex-acp");
        assert!(codex.acp_args.is_empty());
        assert_eq!(codex.callback_policy, CallbackPolicy::AutoApprove);

        let gemini = AgentConfig::gemini_default();
        assert_eq!(gemini.acp_command, "gemini");
        assert_eq!(gemini.acp_args, vec!["--acp"]);

        let opencode = AgentConfig::opencode_default();
        assert_eq!(opencode.acp_command, "opencode");
        assert_eq!(opencode.acp_args, vec!["acp"]);

        let claude = AgentConfig::claudecode_default();
        assert_eq!(claude.acp_command, "claude-agent-acp");
    }

    #[test]
    fn test_agent_config_with_command() {
        let config = AgentConfig::codex_default().with_command("my-codex".to_string());
        assert_eq!(config.acp_command, "my-codex");
    }

    #[test]
    fn test_agent_config_with_command_empty_ignored() {
        let config = AgentConfig::codex_default().with_command(String::new());
        assert_eq!(config.acp_command, "codex-acp");
    }

    #[test]
    fn test_agent_config_with_command_whitespace_ignored() {
        let config = AgentConfig::codex_default().with_command("   ".to_string());
        assert_eq!(config.acp_command, "codex-acp");
    }

    #[test]
    fn test_agent_config_with_args() {
        let config = AgentConfig::gemini_default()
            .with_args(vec!["--yolo".to_string(), "--fast".to_string()]);
        assert_eq!(config.acp_args, vec!["--yolo", "--fast"]);
    }

    #[test]
    fn test_agent_config_extend_args() {
        let config = AgentConfig::codex_default()
            .with_args(vec!["-a".to_string(), "never".to_string()])
            .extend_args(vec!["--read-only".to_string()]);
        assert_eq!(config.acp_args, vec!["-a", "never", "--read-only"]);
    }

    #[test]
    fn test_agent_config_with_callback_policy() {
        let config = AgentConfig::codex_default().with_callback_policy(CallbackPolicy::DenyAll);
        assert_eq!(config.callback_policy, CallbackPolicy::DenyAll);
    }

    #[test]
    fn test_config_default_contains_all_agents() {
        let config = Config::default();
        let expected = ["codex", "gemini", "opencode", "claudecode"];
        for agent in &expected {
            assert!(config.agents.contains_key(*agent));
        }
        assert_eq!(config.agents.len(), expected.len());
    }
}
