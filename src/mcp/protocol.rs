//! MCP Protocol types

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
}

impl JsonRpcMessage {
    pub fn method(&self) -> &str {
        match self {
            JsonRpcMessage::Request(req) => &req.method,
            JsonRpcMessage::Notification(notif) => &notif.method,
        }
    }

    pub fn id(&self) -> Option<&serde_json::Value> {
        match self {
            JsonRpcMessage::Request(req) => Some(&req.id),
            JsonRpcMessage::Notification(_) => None,
        }
    }

    pub fn params(&self) -> &serde_json::Value {
        match self {
            JsonRpcMessage::Request(req) => &req.params,
            JsonRpcMessage::Notification(notif) => &notif.params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: serde_json::Value, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: ProgressParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressParams {
    #[serde(rename = "progressToken")]
    pub progress_token: String,
    pub progress: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_jsonrpc_request_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: json!(1),
            method: "test_method".to_string(),
            params: json!({"key": "value"}),
        };

        let serialized = serde_json::to_string(&req).unwrap();
        assert!(serialized.contains("test_method"));
        assert!(serialized.contains("\"id\":1"));
    }

    #[test]
    fn test_jsonrpc_message_method() {
        let req = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: json!(1),
            method: "initialize".to_string(),
            params: json!({}),
        });

        assert_eq!(req.method(), "initialize");
        assert_eq!(req.id(), Some(&json!(1)));
    }

    #[test]
    fn test_jsonrpc_notification() {
        let notif = JsonRpcMessage::Notification(JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: "initialized".to_string(),
            params: json!({}),
        });

        assert_eq!(notif.method(), "initialized");
        assert_eq!(notif.id(), None);
    }

    #[test]
    fn test_jsonrpc_response_success() {
        let response = JsonRpcResponse::success(json!(1), json!({"status": "ok"}));

        assert_eq!(response.jsonrpc, "2.0");
        assert_eq!(response.id, json!(1));
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    #[test]
    fn test_jsonrpc_response_error() {
        let error = JsonRpcError {
            code: -32600,
            message: "Invalid Request".to_string(),
            data: None,
        };

        let response = JsonRpcResponse::error(json!(1), error);

        assert_eq!(response.jsonrpc, "2.0");
        assert!(response.result.is_none());
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32600);
    }

    #[test]
    fn test_initialize_result() {
        let result = InitializeResult {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ServerCapabilities { tools: None },
            server_info: ServerInfo {
                name: "ccgo".to_string(),
                version: "0.1.0".to_string(),
            },
        };

        let serialized = serde_json::to_value(&result).unwrap();
        assert_eq!(serialized["protocolVersion"], "2024-11-05");
        assert_eq!(serialized["serverInfo"]["name"], "ccgo");
    }

    #[test]
    fn test_tool_definition() {
        let tool = ToolDefinition {
            name: "ask_agent".to_string(),
            description: "Send a message to an AI agent".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "agent_name": {"type": "string"}
                }
            }),
        };

        assert_eq!(tool.name, "ask_agent");

        let serialized = serde_json::to_value(&tool).unwrap();
        assert_eq!(serialized["inputSchema"]["type"], "object");
    }

    #[test]
    fn test_tool_call_params() {
        let params_json = json!({
            "name": "ask_agent",
            "arguments": {
                "agent_name": "codex",
                "message": "Hello"
            }
        });

        let params: ToolCallParams = serde_json::from_value(params_json).unwrap();
        assert_eq!(params.name, "ask_agent");
        assert_eq!(params.arguments["agent_name"], "codex");
    }

    #[test]
    fn test_tool_call_result() {
        let result = ToolCallResult {
            content: vec![ToolContent::Text {
                text: "Response from agent".to_string(),
            }],
            is_error: false,
        };

        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);

        let serialized = serde_json::to_value(&result).unwrap();
        assert_eq!(serialized["isError"], false);
    }

    #[test]
    fn test_tool_content_text() {
        let content = ToolContent::Text {
            text: "Hello, world!".to_string(),
        };

        let serialized = serde_json::to_value(&content).unwrap();
        assert_eq!(serialized["type"], "text");
        assert_eq!(serialized["text"], "Hello, world!");
    }

    #[test]
    fn test_progress_params() {
        let params = ProgressParams {
            progress_token: "token123".to_string(),
            progress: 50.0,
            total: Some(100.0),
        };

        let serialized = serde_json::to_value(&params).unwrap();
        assert_eq!(serialized["progressToken"], "token123");
        assert_eq!(serialized["progress"], 50.0);
        assert_eq!(serialized["total"], 100.0);
    }

    #[test]
    fn test_json_rpc_message_deserialization() {
        // Test request deserialization
        let request_json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "test"}
        }"#;

        let msg: JsonRpcMessage = serde_json::from_str(request_json).unwrap();
        assert_eq!(msg.method(), "tools/call");

        // Test notification deserialization
        let notif_json = r#"{
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }"#;

        let msg: JsonRpcMessage = serde_json::from_str(notif_json).unwrap();
        assert!(msg.id().is_none());
    }
}
