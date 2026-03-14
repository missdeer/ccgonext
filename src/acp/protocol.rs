use agent_client_protocol_schema::RequestId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct JsonRpcRequest<P: Serialize> {
    pub jsonrpc: &'static str,
    pub id: RequestId,
    pub method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
}

impl<P: Serialize> JsonRpcRequest<P> {
    pub fn new(id: RequestId, method: &'static str, params: P) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method,
            params: Some(params),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcNotification<P: Serialize> {
    pub jsonrpc: &'static str,
    pub method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
}

impl<P: Serialize> JsonRpcNotification<P> {
    pub fn new(method: &'static str, params: P) -> Self {
        Self {
            jsonrpc: "2.0",
            method,
            params: Some(params),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct IncomingMessage {
    #[serde(default)]
    pub id: Option<RequestId>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

impl IncomingMessage {
    pub fn classify(self) -> IncomingKind {
        if let Some(id) = self.id {
            if let Some(method) = self.method {
                IncomingKind::Request {
                    id,
                    method,
                    params: self.params.unwrap_or(serde_json::Value::Null),
                }
            } else {
                IncomingKind::Response {
                    id,
                    result: self.result,
                    error: self.error,
                }
            }
        } else if let Some(method) = self.method {
            IncomingKind::Notification {
                method,
                params: self.params.unwrap_or(serde_json::Value::Null),
            }
        } else {
            IncomingKind::Invalid
        }
    }
}

#[derive(Debug)]
pub enum IncomingKind {
    Request {
        id: RequestId,
        method: String,
        params: serde_json::Value,
    },
    Response {
        id: RequestId,
        result: Option<serde_json::Value>,
        error: Option<JsonRpcError>,
    },
    Notification {
        method: String,
        params: serde_json::Value,
    },
    Invalid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_classify_response() {
        let msg: IncomingMessage = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"ok": true}
        }))
        .unwrap();
        match msg.classify() {
            IncomingKind::Response { id, result, error } => {
                assert!(matches!(id, RequestId::Number(1)));
                assert!(result.is_some());
                assert!(error.is_none());
            }
            other => panic!("Expected Response, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_request() {
        let msg: IncomingMessage = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "fs/readTextFile",
            "params": {"path": "/tmp/test.txt"}
        }))
        .unwrap();
        match msg.classify() {
            IncomingKind::Request { id, method, params } => {
                assert!(matches!(id, RequestId::Number(5)));
                assert_eq!(method, "fs/readTextFile");
                assert!(params.is_object());
            }
            other => panic!("Expected Request, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_notification() {
        let msg: IncomingMessage = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"data": 1}
        }))
        .unwrap();
        match msg.classify() {
            IncomingKind::Notification { method, params } => {
                assert_eq!(method, "session/update");
                assert!(params.is_object());
            }
            other => panic!("Expected Notification, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_notification_no_params() {
        let msg: IncomingMessage = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "method": "session/cancel"
        }))
        .unwrap();
        match msg.classify() {
            IncomingKind::Notification { method, params } => {
                assert_eq!(method, "session/cancel");
                assert!(params.is_null());
            }
            other => panic!("Expected Notification, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_invalid() {
        let msg: IncomingMessage = serde_json::from_value(json!({
            "jsonrpc": "2.0"
        }))
        .unwrap();
        assert!(matches!(msg.classify(), IncomingKind::Invalid));
    }

    #[test]
    fn test_classify_error_response() {
        let msg: IncomingMessage = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "error": {"code": -32600, "message": "Invalid request"}
        }))
        .unwrap();
        match msg.classify() {
            IncomingKind::Response { error, result, .. } => {
                assert!(error.is_some());
                assert_eq!(error.unwrap().code, -32600);
                assert!(result.is_none());
            }
            other => panic!("Expected Response, got {:?}", other),
        }
    }

    #[test]
    fn test_request_serialization() {
        let req = JsonRpcRequest::new(RequestId::Number(1), "initialize", json!({"version": "1"}));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert_eq!(json["method"], "initialize");
        assert!(json["params"].is_object());
    }

    #[test]
    fn test_notification_serialization() {
        let notif = JsonRpcNotification::new("session/cancel", json!({"session_id": "abc"}));
        let json = serde_json::to_value(&notif).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["method"], "session/cancel");
        assert!(json.get("id").is_none());
    }
}
