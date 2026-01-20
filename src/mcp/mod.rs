//! MCP Server implementation

mod protocol;
mod tools;

pub use protocol::*;
pub use tools::*;

use crate::config::Config;
use crate::session::SessionManager;
use crossterm::terminal;
use std::io::IsTerminal;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// Transport mode for MCP protocol
#[derive(Debug, Clone, Copy, PartialEq)]
enum TransportMode {
    /// Auto-detect mode (initial state)
    AutoDetect,
    /// Line-delimited JSON (JSONL) - one JSON object per line
    JsonLines,
    /// LSP-style with Content-Length header
    LspStyle,
}

/// Maximum allowed Content-Length (16MB) to prevent OOM attacks
const MAX_CONTENT_LENGTH: usize = 16 * 1024 * 1024;

pub struct McpServer {
    session_manager: Arc<SessionManager>,
    #[allow(dead_code)]
    config: Arc<Config>,
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

impl McpServer {
    pub fn new(session_manager: Arc<SessionManager>, config: Arc<Config>) -> Self {
        Self {
            session_manager,
            config,
        }
    }

    pub async fn run_stdio(&self) -> anyhow::Result<()> {
        let _raw_mode_guard = if std::io::stdin().is_terminal() {
            match terminal::enable_raw_mode() {
                Ok(()) => Some(RawModeGuard),
                Err(e) => {
                    tracing::warn!("Failed to enable raw mode on stdin: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let stdin = tokio::io::stdin();
        let stdout = Arc::new(Mutex::new(tokio::io::stdout()));
        let mut reader = BufReader::new(stdin);
        let mut mode = TransportMode::AutoDetect;

        tracing::info!("MCP Server started on stdio (auto-detecting transport mode)");

        loop {
            let message_result = match mode {
                TransportMode::AutoDetect => {
                    // Peek at first bytes to detect mode
                    let detected = self.detect_and_read_message(&mut reader, &mut mode).await;
                    detected
                }
                TransportMode::JsonLines => self.read_jsonl_message(&mut reader).await,
                TransportMode::LspStyle => self.read_lsp_message(&mut reader).await,
            };

            match message_result {
                Ok(None) => break, // EOF
                Ok(Some(content)) => {
                    let content = content.trim();
                    if content.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<JsonRpcMessage>(content) {
                        Ok(message) => {
                            if let Some(response) = self.handle_message(message).await {
                                self.write_response(&stdout, &response, mode).await?;
                            }
                        }
                        Err(e) => {
                            let error_response = JsonRpcResponse::error(
                                serde_json::Value::Null,
                                JsonRpcError {
                                    code: -32700,
                                    message: format!("Parse error: {}", e),
                                    data: None,
                                },
                            );
                            self.write_response(&stdout, &error_response, mode).await?;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Error reading stdin: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }

    /// Detect transport mode from first message and read it
    async fn detect_and_read_message<R: AsyncBufReadExt + Unpin>(
        &self,
        reader: &mut R,
        mode: &mut TransportMode,
    ) -> anyhow::Result<Option<String>> {
        let mut first_line = String::new();
        let bytes_read = reader.read_line(&mut first_line).await?;

        if bytes_read == 0 {
            return Ok(None); // EOF
        }

        let trimmed = first_line.trim();

        // Check if it looks like LSP Content-Length header
        if trimmed.to_lowercase().starts_with("content-length:") {
            *mode = TransportMode::LspStyle;
            tracing::info!("Detected LSP-style transport mode");

            // Parse content length
            let content_length = self.parse_content_length(trimmed)?;

            // Read until empty line (end of headers)
            loop {
                let mut header_line = String::new();
                reader.read_line(&mut header_line).await?;
                if header_line.trim().is_empty() {
                    break;
                }
                // Could parse other headers here if needed
            }

            // Read the JSON content
            let mut content = vec![0u8; content_length];
            reader.read_exact(&mut content).await?;
            Ok(Some(String::from_utf8(content)?))
        } else if trimmed.starts_with('{') {
            // Looks like JSON, use JSONL mode
            *mode = TransportMode::JsonLines;
            tracing::info!("Detected JSON Lines transport mode");
            Ok(Some(first_line))
        } else if trimmed.is_empty() {
            // Empty line, keep auto-detecting
            Ok(Some(String::new()))
        } else {
            // Unknown format, try JSONL
            *mode = TransportMode::JsonLines;
            tracing::warn!(
                "Unknown format, assuming JSON Lines mode. First line: {}",
                trimmed
            );
            Ok(Some(first_line))
        }
    }

    /// Read a message in JSONL mode (line-delimited)
    async fn read_jsonl_message<R: AsyncBufReadExt + Unpin>(
        &self,
        reader: &mut R,
    ) -> anyhow::Result<Option<String>> {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;

        if bytes_read == 0 {
            return Ok(None); // EOF
        }

        Ok(Some(line))
    }

    /// Read a message in LSP mode (Content-Length header)
    async fn read_lsp_message<R: AsyncBufReadExt + AsyncReadExt + Unpin>(
        &self,
        reader: &mut R,
    ) -> anyhow::Result<Option<String>> {
        let mut content_length: Option<usize> = None;

        // Read headers
        loop {
            let mut header_line = String::new();
            let bytes_read = reader.read_line(&mut header_line).await?;

            if bytes_read == 0 {
                return Ok(None); // EOF
            }

            let trimmed = header_line.trim();
            if trimmed.is_empty() {
                // End of headers
                break;
            }

            if trimmed.to_lowercase().starts_with("content-length:") {
                content_length = Some(self.parse_content_length(trimmed)?);
            }
            // Ignore other headers (Content-Type, etc.)
        }

        let length = content_length
            .ok_or_else(|| anyhow::anyhow!("Missing Content-Length header in LSP message"))?;

        // Read the JSON content
        let mut content = vec![0u8; length];
        reader.read_exact(&mut content).await?;

        Ok(Some(String::from_utf8(content)?))
    }

    /// Parse Content-Length header value
    fn parse_content_length(&self, header: &str) -> anyhow::Result<usize> {
        let parts: Vec<&str> = header.splitn(2, ':').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid Content-Length header format");
        }

        let length: usize = parts[1].trim().parse()?;

        // Prevent OOM attacks by limiting content length
        if length > MAX_CONTENT_LENGTH {
            anyhow::bail!(
                "Content-Length {} exceeds maximum allowed size {}",
                length,
                MAX_CONTENT_LENGTH
            );
        }

        Ok(length)
    }

    /// Write response in the appropriate format
    async fn write_response(
        &self,
        stdout: &Arc<Mutex<tokio::io::Stdout>>,
        response: &JsonRpcResponse,
        mode: TransportMode,
    ) -> anyhow::Result<()> {
        let response_json = serde_json::to_string(response)?;
        let mut out = stdout.lock().await;

        match mode {
            TransportMode::LspStyle => {
                // LSP style: Content-Length header + \r\n\r\n + content
                let header = format!("Content-Length: {}\r\n\r\n", response_json.len());
                out.write_all(header.as_bytes()).await?;
                out.write_all(response_json.as_bytes()).await?;
            }
            TransportMode::JsonLines | TransportMode::AutoDetect => {
                // JSONL style: JSON + newline
                out.write_all(response_json.as_bytes()).await?;
                out.write_all(b"\n").await?;
            }
        }

        out.flush().await?;
        Ok(())
    }

    async fn handle_message(&self, message: JsonRpcMessage) -> Option<JsonRpcResponse> {
        match message {
            JsonRpcMessage::Request(request) => Some(self.handle_request(request).await),
            JsonRpcMessage::Notification(notification) => {
                self.handle_notification(notification).await;
                None // Notifications don't get responses
            }
        }
    }

    async fn handle_notification(&self, notification: JsonRpcNotification) {
        match notification.method.as_str() {
            "notifications/initialized" => {
                tracing::info!("Client initialized");
            }
            "notifications/cancelled" => {
                tracing::debug!("Request cancelled");
            }
            _ => {
                tracing::debug!("Unknown notification: {}", notification.method);
            }
        }
    }

    async fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request).await,
            "tools/list" => self.handle_tools_list(request).await,
            "tools/call" => self.handle_tools_call(request).await,
            _ => JsonRpcResponse::error(
                request.id,
                JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                    data: None,
                },
            ),
        }
    }

    async fn handle_initialize(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let result = InitializeResult {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability { list_changed: true }),
            },
            server_info: ServerInfo {
                name: "ccgo".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        JsonRpcResponse::success(request.id, serde_json::to_value(result).unwrap())
    }

    async fn handle_tools_list(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let tools = get_tool_definitions();
        let result = ToolsListResult { tools };
        JsonRpcResponse::success(request.id, serde_json::to_value(result).unwrap())
    }

    async fn handle_tools_call(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let params: ToolCallParams = match serde_json::from_value(request.params) {
            Ok(p) => p,
            Err(e) => {
                return JsonRpcResponse::error(
                    request.id,
                    JsonRpcError {
                        code: -32602,
                        message: format!("Invalid params: {}", e),
                        data: None,
                    },
                );
            }
        };

        let result = execute_tool(&params.name, params.arguments, &self.session_manager).await;

        match result {
            Ok(content) => {
                let tool_result = ToolCallResult {
                    content: vec![ToolContent::Text { text: content }],
                    is_error: false,
                };
                JsonRpcResponse::success(request.id, serde_json::to_value(tool_result).unwrap())
            }
            Err(e) => {
                let tool_result = ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: e.to_string(),
                    }],
                    is_error: true,
                };
                JsonRpcResponse::success(request.id, serde_json::to_value(tool_result).unwrap())
            }
        }
    }
}
