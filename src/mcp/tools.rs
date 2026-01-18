//! MCP Tool implementations

use super::ToolDefinition;
use crate::session::SessionManager;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "ask_agent".to_string(),
        description: "Send a message to an AI agent and wait for response. Agent is auto-started if not running.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "description": "Name of the agent (codex, gemini, opencode)"
                },
                "message": {
                    "type": "string",
                    "description": "Message to send to the agent"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (optional, default: 600)"
                }
            },
            "required": ["agent_name", "message"]
        }),
    }]
}

pub async fn execute_tool(
    name: &str,
    args: serde_json::Value,
    session_manager: &Arc<SessionManager>,
) -> Result<String, anyhow::Error> {
    match name {
        "ask_agent" => {
            let agent_name = args
                .get("agent_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing agent_name"))?;

            let message = args
                .get("message")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing message"))?;

            let timeout = args
                .get("timeout")
                .and_then(|v| v.as_u64())
                .map(Duration::from_secs);

            let session = session_manager
                .get(agent_name)
                .await
                .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", agent_name))?;

            let pty_manager = session_manager.pty_manager();
            let response = session
                .ask(message.to_string(), timeout, pty_manager)
                .await?;

            Ok(response)
        }
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    }
}
