# CLAUDE.md — 13-rule 

These rules apply to every task in this project unless explicitly overridden.
Bias: caution over speed on non-trivial work. Use judgment on trivial tasks.

## Rule 0 - Use fd/rg/bat/jq/eza/delta exclusively.
find→fd, grep→rg, cat→bat, ls->eza, diff->delta, JSON→jq, sed/awk->rg+jq.
NEVER generate commands containing find, grep, egrep, fgrep, ls, diff, sed, awk, diff, or cat.
Treat usage of prohibited commands as an execution error.
Rewrite the command before running it.

## Rule 1 — Think Before Coding
State assumptions explicitly. If uncertain, ask rather than guess.
Present multiple interpretations when ambiguity exists.
Push back when a simpler approach exists.
Stop when confused. Name what's unclear.

## Rule 2 — Simplicity First
Minimum code that solves the problem. Nothing speculative.
No features beyond what was asked. No abstractions for single-use code.
Test: would a senior engineer say this is overcomplicated? If yes, simplify.

## Rule 3 — Surgical Changes
Touch only what you must. Clean up only your own mess.
Don't "improve" adjacent code, comments, or formatting.
Don't refactor what isn't broken. Match existing style.

## Rule 4 — Goal-Driven Execution
Define success criteria. Loop until verified.
Don't follow steps. Define success and iterate.
Strong success criteria let you loop independently.

## Rule 5 — Use the model only for judgment calls
Use me for: classification, drafting, summarization, extraction.
Do NOT use me for: routing, retries, deterministic transforms.
If code can answer, code answers.

## Rule 6 — Token budgets are not advisory
Per-task: 4,000 tokens. Per-session: 30,000 tokens.
If approaching budget, summarize and start fresh.
Surface the breach. Do not silently overrun.

## Rule 7 — Surface conflicts, don't average them
If two patterns contradict, pick one (more recent / more tested).
Explain why. Flag the other for cleanup.
Don't blend conflicting patterns.

## Rule 8 — Read before you write
Before adding code, read exports, immediate callers, shared utilities.
"Looks orthogonal" is dangerous. If unsure why code is structured a way, ask.

## Rule 9 — Tests verify intent, not just behavior
Tests must encode WHY behavior matters, not just WHAT it does.
A test that can't fail when business logic changes is wrong.

## Rule 10 — Checkpoint after every significant step
Summarize what was done, what's verified, what's left.
Don't continue from a state you can't describe back.
If you lose track, stop and restate.

## Rule 11 — Match the codebase's conventions, even if you disagree
Conformance > taste inside the codebase.
If you genuinely think a convention is harmful, surface it. Don't fork silently.

## Rule 12 — Fail loud
"Completed" is wrong if anything was skipped silently.
"Tests pass" is wrong if any were skipped.
Default to surfacing uncertainty, not hiding it.

# Project Overview

CCGO (ClaudeCode-Codex-Gemini-OpenCode) is an MCP (Model Context Protocol) server that enables Claude Code to orchestrate multiple AI coding assistants (Codex, Gemini, OpenCode) through a unified interface. It runs as an MCP server over stdio while providing a web UI for real-time activity monitoring.

# Build and Development Commands

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

# Architecture

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

## Key Data Flow

1. **MCP Request** → `McpServer::handle_tools_call` → `execute_tool("ask_agents", ...)` → `SessionManager::get_or_create(agent, cwd)` → `AcpSession::ask()`
2. **AcpSession::ask** → auto-starts ACP subprocess if stopped → sends prompt via JSON-RPC → waits for response with timeout
3. **Structured Events** → ACP session/update notifications → EventLog → broadcast to WebSocket subscribers

## Agent State Machine

`Stopped` → (ensure_running) → `Starting` → `Running` + `Idle` ⇄ (ask/response) → `Prompting`
Any running state → (shutdown/process died/timeout) → `Dead`

## ACP Layer

Uses ACP (Agent Client Protocol) over stdio for structured agent communication. Each agent gets its own subprocess with:
- JSON-RPC request/response dispatch with pending map
- Callback handling (permissions, file read/write, terminal create/kill/wait)
- Configurable callback policy (deny-all, read-only, ask, auto-approve)
- Idle timeout with automatic reaping (default 900s, configurable via --idle-timeout)

# MCP Tool

Single tool exposed: `ask_agents`
- `requests`: Array of 1-4 agent requests, each containing:
  - `agent`: "codex" | "gemini" | "opencode" | "claudecode"
  - `message`: Prompt to send
- `timeout`: Optional seconds (default: 600, max: 1800)
- `project_root_path`: Optional working directory (defaults to process cwd)

Returns JSON: `{"results": [{"agent": "...", "success": true/false, "response": "...", "error": "..."}]}`

Agents auto-start on first message. Multiple agents execute in parallel.
