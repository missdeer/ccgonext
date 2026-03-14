# CLAUDE 

## Workflow

- **MUST** follow multi-agent-workflow
- carefully read and understand rules defined in `.claude/rules` directory and follow them strictly

---

## File editing on Windows (CRITICAL FIX)

**ALWAYS use RELATIVE paths** for Read and Edit tools:

✅ CORRECT:
- Read("src/components/Button.tsx")
- Edit("src/components/Button.tsx", ...)
- Read("config/settings.json")
- Edit("config/settings.json", ...)

❌ INCORRECT:
- Read("C:/Users/.../src/components/Button.tsx")
- Edit("C:/Users/.../src/components/Button.tsx", ...)

**Rules:**
1. Use paths relative to your working directory
2. Use the SAME exact path in Read and Edit
3. Avoid absolute paths with forward slashes

**If error persists:** Re-read with the SAME relative path.

---

## Project Overview

CCGO (ClaudeCode-Codex-Gemini-OpenCode) is an MCP (Model Context Protocol) server that enables Claude Code to orchestrate multiple AI coding assistants (Codex, Gemini, OpenCode) through a unified interface. It runs as an MCP server over stdio while providing a web UI for real-time activity monitoring.

## Build and Development Commands

```bash
# Build
cargo build                    # Debug build
cargo build --release          # Release build (optimized, stripped)

# Run
cargo run -- serve             # Run as MCP server (default)
cargo run -- web               # Run web server only
cargo run -- config            # Show configuration

# Testing
cargo test --lib --verbose           # Unit tests
cargo test --test '*' --verbose      # Integration tests
cargo test --doc --verbose           # Doc tests
cargo test --all-features --verbose  # All tests with all features

# Linting and Formatting
cargo fmt --all -- --check                              # Check formatting
cargo clippy --all-targets --all-features -- -D warnings  # Lint
cargo doc --no-deps --all-features                      # Build docs (RUSTDOCFLAGS=-D warnings)

# Static check
cargo check --all-targets --all-features
```

Minimum supported Rust version: 1.90

## Architecture

```
src/
├── main.rs          # CLI entry point (clap), builds Config, launches servers
├── lib.rs           # Public module exports
├── config/          # Configuration structs (ServerConfig, AgentConfig, TimeoutConfig, WebConfig)
├── acp/             # ACP (Agent Client Protocol) client over stdio
│   ├── mod.rs       # AcpClient: JSON-RPC over stdin/stdout, request/response dispatch
│   ├── callbacks.rs # CallbackHandler: permission, file I/O, terminal callbacks
│   ├── process.rs   # AcpProcess: subprocess spawn and stdio pipe setup
│   └── protocol.rs  # JSON-RPC message types, classification, serialization
├── session/         # Session management layer
│   └── mod.rs       # AcpSession (per-agent state machine), SessionManager (registry)
├── events.rs        # Replayable event log with broadcast (std::sync locks)
├── mcp/             # MCP protocol implementation
│   ├── mod.rs       # McpServer: stdio JSON-RPC (auto-detects JSONL vs LSP-style)
│   ├── protocol.rs  # JSON-RPC types, MCP initialize/tools/call handlers
│   └── tools.rs     # Tool definitions (ask_agents)
├── state/           # Agent state machine (Stopped→Starting→Running, Idle⇄Prompting→Dead)
└── web/             # Web UI and API (axum)
    ├── mod.rs       # WebServer, routes (/api/*, /ws/*)
    ├── handlers.rs  # REST API handlers
    ├── websocket.rs # WebSocket event streaming
    └── static_files.rs  # Embedded static files (rust-embed)
```

### Key Data Flow

1. **MCP Request** → `McpServer::handle_tools_call` → `execute_tool("ask_agents", ...)` → `SessionManager::get_or_create(agent, cwd)` → `AcpSession::ask()`
2. **AcpSession::ask** → auto-starts ACP subprocess if stopped → sends prompt via JSON-RPC → waits for response with timeout
3. **Structured Events** → ACP session/update notifications → EventLog → broadcast to WebSocket subscribers

### Agent State Machine

`Stopped` → (ensure_running) → `Starting` → `Running` + `Idle` ⇄ (ask/response) → `Prompting`
Any running state → (shutdown/process died/timeout) → `Dead`

### ACP Layer

Uses ACP (Agent Client Protocol) over stdio for structured agent communication. Each agent gets its own subprocess with:
- JSON-RPC request/response dispatch with pending map
- Callback handling (permissions, file read/write, terminal create/kill/wait)
- Configurable callback policy (deny-all, read-only, ask, auto-approve)
- Idle timeout with automatic reaping (default 900s, configurable via --idle-timeout)

## MCP Tool

Single tool exposed: `ask_agents`
- `requests`: Array of 1-4 agent requests, each containing:
  - `agent`: "codex" | "gemini" | "opencode" | "claudecode"
  - `message`: Prompt to send
- `timeout`: Optional seconds (default: 600, max: 1800)
- `project_root_path`: Optional working directory (defaults to process cwd)

Returns JSON: `{"results": [{"agent": "...", "success": true/false, "response": "...", "error": "..."}]}`

Agents auto-start on first message. Multiple agents execute in parallel.
