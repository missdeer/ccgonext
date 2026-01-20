//! MCP Tool implementations

use super::ToolDefinition;
use crate::session::SessionManager;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

const VALID_AGENTS: &[&str] = &["codex", "gemini", "opencode", "claudecode"];
const DEFAULT_TIMEOUT: u64 = 600;
const MAX_TIMEOUT: u64 = 1800;
const MAX_REQUESTS: usize = 4;

#[derive(Debug, Deserialize)]
pub struct AskAgentsArgs {
    pub requests: Vec<AgentRequest>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Deserialize)]
pub struct AgentRequest {
    pub agent: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AskAgentsResponse {
    pub results: Vec<AgentResult>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentResult {
    pub agent: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT
}

fn validate_args(args: &AskAgentsArgs) -> Result<(), anyhow::Error> {
    if args.requests.is_empty() || args.requests.len() > MAX_REQUESTS {
        anyhow::bail!("requests must have 1-{} items", MAX_REQUESTS);
    }

    let mut seen = HashSet::new();
    for req in &args.requests {
        if !seen.insert(&req.agent) {
            anyhow::bail!("duplicate agent: {}", req.agent);
        }
    }

    for req in &args.requests {
        if !VALID_AGENTS.contains(&req.agent.as_str()) {
            anyhow::bail!("invalid agent: {}", req.agent);
        }
        if req.message.trim().is_empty() {
            anyhow::bail!("message cannot be empty for agent: {}", req.agent);
        }
    }

    if args.timeout == 0 || args.timeout > MAX_TIMEOUT {
        anyhow::bail!("timeout must be 1-{} seconds", MAX_TIMEOUT);
    }

    Ok(())
}

pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "ask_agents".to_string(),
        description: "Send messages to AI agents in parallel and wait for responses. Agents are auto-started if not running.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "requests": {
                    "type": "array",
                    "description": "Agent requests (1-4 items)",
                    "minItems": 1,
                    "maxItems": 4,
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": {
                                "type": "string",
                                "enum": ["codex", "gemini", "opencode", "claudecode"],
                                "description": "Name of the agent"
                            },
                            "message": {
                                "type": "string",
                                "description": "Message to send to the agent"
                            }
                        },
                        "required": ["agent", "message"]
                    }
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 600, max: 1800)"
                }
            },
            "required": ["requests"]
        }),
    }]
}

pub async fn execute_tool(
    name: &str,
    args: serde_json::Value,
    session_manager: &Arc<SessionManager>,
) -> Result<String, anyhow::Error> {
    match name {
        "ask_agents" => {
            let args: AskAgentsArgs = serde_json::from_value(args)?;
            execute_ask_agents(args, session_manager).await
        }
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    }
}

async fn execute_ask_agents(
    args: AskAgentsArgs,
    session_manager: &Arc<SessionManager>,
) -> Result<String, anyhow::Error> {
    validate_args(&args)?;

    let timeout_duration = Duration::from_secs(args.timeout);
    let request_count = args.requests.len();

    // Pre-collect agent names for error fallback
    let agent_names: Vec<String> = args.requests.iter().map(|r| r.agent.clone()).collect();

    let mut join_set = JoinSet::new();
    // Map task ID to request index for JoinError attribution
    let mut task_id_to_idx: HashMap<tokio::task::Id, usize> = HashMap::new();

    for (idx, req) in args.requests.into_iter().enumerate() {
        let sm = session_manager.clone();
        let agent = req.agent.clone();
        let message = req.message.clone();

        let handle = join_set.spawn(async move {
            let result = AssertUnwindSafe(async {
                // Pass timeout to ask_single_agent to ensure ReplyDetection uses it
                // This prevents ReplyDetection from continuing beyond the MCP timeout
                ask_single_agent(&agent, &message, Some(timeout_duration), &sm).await
            })
            .catch_unwind()
            .await;

            let agent_result = match result {
                Ok(Ok(response)) => AgentResult {
                    agent: agent.clone(),
                    success: true,
                    response: Some(response),
                    error: None,
                },
                Ok(Err(e)) => AgentResult {
                    agent: agent.clone(),
                    success: false,
                    response: None,
                    error: Some(e.to_string()),
                },
                Err(panic_err) => {
                    let panic_msg = if let Some(s) = panic_err.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_err.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    AgentResult {
                        agent: agent.clone(),
                        success: false,
                        response: None,
                        error: Some(format!("task panicked: {}", panic_msg)),
                    }
                }
            };
            (idx, agent_result)
        });
        task_id_to_idx.insert(handle.id(), idx);
    }

    let mut results: Vec<Option<AgentResult>> = vec![None; request_count];

    while let Some(result) = join_set.join_next_with_id().await {
        match result {
            Ok((_task_id, (idx, agent_result))) => {
                results[idx] = Some(agent_result);
            }
            Err(join_error) => {
                // JoinError - use task ID from error to find the correct index
                let task_id = join_error.id();
                if let Some(&idx) = task_id_to_idx.get(&task_id) {
                    results[idx] = Some(AgentResult {
                        agent: agent_names[idx].clone(),
                        success: false,
                        response: None,
                        error: Some(format!("task cancelled: {}", join_error)),
                    });
                } else {
                    tracing::error!("Unknown task ID in JoinError: {:?}", task_id);
                }
            }
        }
    }

    // Convert to final results - use pre-collected agent names for any remaining None slots
    let results: Vec<AgentResult> = results
        .into_iter()
        .enumerate()
        .map(|(idx, opt)| {
            opt.unwrap_or_else(|| {
                tracing::error!("Result slot {} was not filled", idx);
                AgentResult {
                    agent: agent_names[idx].clone(),
                    success: false,
                    response: None,
                    error: Some("internal error: result not collected".to_string()),
                }
            })
        })
        .collect();

    let response = AskAgentsResponse { results };
    Ok(serde_json::to_string(&response)?)
}

async fn ask_single_agent(
    agent_name: &str,
    message: &str,
    timeout: Option<Duration>,
    session_manager: &Arc<SessionManager>,
) -> Result<String, anyhow::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ask_agents_args_parsing() {
        let json = json!({
            "requests": [
                {"agent": "codex", "message": "hello"},
                {"agent": "gemini", "message": "world"}
            ],
            "timeout": 300
        });

        let args: AskAgentsArgs = serde_json::from_value(json).unwrap();
        assert_eq!(args.requests.len(), 2);
        assert_eq!(args.timeout, 300);
    }

    #[test]
    fn test_ask_agents_args_default_timeout() {
        let json = json!({
            "requests": [{"agent": "codex", "message": "hello"}]
        });

        let args: AskAgentsArgs = serde_json::from_value(json).unwrap();
        assert_eq!(args.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn test_validate_args_empty_requests() {
        let args = AskAgentsArgs {
            requests: vec![],
            timeout: 600,
        };
        assert!(validate_args(&args).is_err());
    }

    #[test]
    fn test_validate_args_too_many_requests() {
        let args = AskAgentsArgs {
            requests: vec![
                AgentRequest {
                    agent: "codex".to_string(),
                    message: "a".to_string(),
                },
                AgentRequest {
                    agent: "gemini".to_string(),
                    message: "b".to_string(),
                },
                AgentRequest {
                    agent: "opencode".to_string(),
                    message: "c".to_string(),
                },
                AgentRequest {
                    agent: "claudecode".to_string(),
                    message: "d".to_string(),
                },
                AgentRequest {
                    agent: "extra".to_string(),
                    message: "e".to_string(),
                },
            ],
            timeout: 600,
        };
        assert!(validate_args(&args).is_err());
    }

    #[test]
    fn test_validate_args_duplicate_agents() {
        let args = AskAgentsArgs {
            requests: vec![
                AgentRequest {
                    agent: "codex".to_string(),
                    message: "a".to_string(),
                },
                AgentRequest {
                    agent: "codex".to_string(),
                    message: "b".to_string(),
                },
            ],
            timeout: 600,
        };
        let err = validate_args(&args).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn test_validate_args_invalid_agent() {
        let args = AskAgentsArgs {
            requests: vec![AgentRequest {
                agent: "invalid_agent".to_string(),
                message: "hello".to_string(),
            }],
            timeout: 600,
        };
        let err = validate_args(&args).unwrap_err();
        assert!(err.to_string().contains("invalid agent"));
    }

    #[test]
    fn test_validate_args_empty_message() {
        let args = AskAgentsArgs {
            requests: vec![AgentRequest {
                agent: "codex".to_string(),
                message: "   ".to_string(),
            }],
            timeout: 600,
        };
        let err = validate_args(&args).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn test_validate_args_invalid_timeout() {
        let args = AskAgentsArgs {
            requests: vec![AgentRequest {
                agent: "codex".to_string(),
                message: "hello".to_string(),
            }],
            timeout: 0,
        };
        assert!(validate_args(&args).is_err());

        let args2 = AskAgentsArgs {
            requests: vec![AgentRequest {
                agent: "codex".to_string(),
                message: "hello".to_string(),
            }],
            timeout: MAX_TIMEOUT + 1,
        };
        assert!(validate_args(&args2).is_err());
    }

    #[test]
    fn test_validate_args_valid() {
        let args = AskAgentsArgs {
            requests: vec![
                AgentRequest {
                    agent: "codex".to_string(),
                    message: "hello".to_string(),
                },
                AgentRequest {
                    agent: "gemini".to_string(),
                    message: "world".to_string(),
                },
            ],
            timeout: 600,
        };
        assert!(validate_args(&args).is_ok());
    }

    #[test]
    fn test_agent_result_serialization() {
        let result = AgentResult {
            agent: "codex".to_string(),
            success: true,
            response: Some("hello".to_string()),
            error: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["agent"], "codex");
        assert_eq!(json["success"], true);
        assert_eq!(json["response"], "hello");
        assert!(json.get("error").is_none());
    }

    #[test]
    fn test_agent_result_error_serialization() {
        let result = AgentResult {
            agent: "gemini".to_string(),
            success: false,
            response: None,
            error: Some("timeout".to_string()),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], false);
        assert_eq!(json["error"], "timeout");
        assert!(json.get("response").is_none());
    }

    #[test]
    fn test_ask_agents_response_serialization() {
        let response = AskAgentsResponse {
            results: vec![
                AgentResult {
                    agent: "codex".to_string(),
                    success: true,
                    response: Some("ok".to_string()),
                    error: None,
                },
                AgentResult {
                    agent: "gemini".to_string(),
                    success: false,
                    response: None,
                    error: Some("error".to_string()),
                },
            ],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("codex"));
        assert!(json.contains("gemini"));
    }
}
