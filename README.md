# CCGONEXT

[中文文档](README_zh.md)

CCGONEXT is an MCP server that lets Claude Code orchestrate multiple coding agents through a single MCP tool while exposing a structured ACP activity UI in the browser.

## What It Does

- Runs as an MCP server over stdio
- Talks to agents through ACP (Agent Client Protocol) over stdio, not PTY mirroring
- Supports `codex`, `gemini`, `opencode`, and `claudecode`
- Keeps per-agent, per-working-directory sessions
- Streams structured events to the web UI: messages, thoughts, plans, tool calls, permissions, and state changes
- Lets the browser send follow-up prompts to existing sessions
- Supports configurable ACP callback policy per agent
- Ships as a single binary with embedded static assets

## Supported Agent Commands

Default ACP commands:

- `codex` -> `codex-acp`
- `gemini` -> `gemini --acp`
- `opencode` -> `opencode acp`
- `claudecode` -> `claude-agent-acp`

You can override any of these with CLI flags or environment variables.

## Installation

### From Source

```bash
cargo build --release
```

Binary output:

- Linux/macOS: `target/release/ccgonext`
- Windows: `target/release/ccgonext.exe`

### Prebuilt Binaries

Download from [Releases](https://github.com/missdeer/ccgonext/releases).

## Quick Start

### Use with Claude Code

Add this to your Claude Code MCP config:

```json
{
  "mcpServers": {
    "ccgonext": {
      "command": "ccgonext",
      "args": ["serve"]
    }
  }
}
```

### Run the Web UI

```bash
ccgonext web --open-browser
```

### Inspect the Effective Config

```bash
ccgonext config
```

## Commands

```text
ccgonext [OPTIONS] [COMMAND]

Commands:
  serve   Run as MCP server (stdio mode) with web UI
  web     Run web server only (standalone mode)
  config  Show current configuration
```

Key options:

| Option | Env | Default |
|---|---|---|
| `--port` | `CCGONEXT_PORT` | `8765` |
| `--host` | `CCGONEXT_HOST` | `127.0.0.1` |
| `--port-retry` | `CCGONEXT_PORT_RETRY` | `0` |
| `--open-browser` | `CCGONEXT_OPEN_BROWSER` | `false` |
| `--auth-token` | `CCGONEXT_AUTH_TOKEN` | unset |
| `--timeout` | `CCGONEXT_TIMEOUT` | `600` |
| `--idle-timeout` | `CCGONEXT_IDLE_TIMEOUT` | `900` |
| `--agents` | `CCGONEXT_AGENTS` | `codex,gemini,opencode` |
| `--codex-cmd` | `CCGONEXT_CODEX_CMD` | `codex-acp` |
| `--gemini-cmd` | `CCGONEXT_GEMINI_CMD` | `gemini` |
| `--opencode-cmd` | `CCGONEXT_OPENCODE_CMD` | `opencode` |
| `--claudecode-cmd` | `CCGONEXT_CLAUDECODE_CMD` | `claude-agent-acp` |
| `--callback-policy` | `CCGONEXT_CALLBACK_POLICY` | `auto-approve` |
| `--codex-callback-policy` | `CCGONEXT_CODEX_CALLBACK_POLICY` | inherit global |
| `--gemini-callback-policy` | `CCGONEXT_GEMINI_CALLBACK_POLICY` | inherit global |
| `--opencode-callback-policy` | `CCGONEXT_OPENCODE_CALLBACK_POLICY` | inherit global |
| `--claudecode-callback-policy` | `CCGONEXT_CLAUDECODE_CALLBACK_POLICY` | inherit global |
| `--log-file` | `CCGONEXT_LOG_FILE` | unset |
| `--log-dir` | `CCGONEXT_LOG_DIR` | unset |

For the full generated help:

```bash
ccgonext --help
```

## Callback Policy

ACP agents can request permissions, file access, and terminal execution through callbacks. CCGONEXT supports four policies:

- `deny-all`
- `read-only`
- `ask`
- `auto-approve`

Current default:

- All enabled agents default to `auto-approve`

Examples:

```bash
# Default behavior
ccgonext serve

# One policy for all enabled agents
ccgonext serve --callback-policy read-only

# Override a single agent
ccgonext serve --callback-policy read-only --codex-callback-policy auto-approve
```

Environment variable equivalents:

```bash
export CCGONEXT_CALLBACK_POLICY=read-only
export CCGONEXT_CODEX_CALLBACK_POLICY=auto-approve
export CCGONEXT_GEMINI_CALLBACK_POLICY=ask
```

`auto-approve` is convenient, but it also means the agent can automatically approve ACP actions such as file writes and terminal execution. If that is too permissive for your environment, use `read-only`, `ask`, or `deny-all`.

## MCP Tool

CCGONEXT exposes one MCP tool: `ask_agents`.

### `ask_agents`

Send prompts to 1-4 agents in parallel.

Request:

```json
{
  "requests": [
    { "agent": "codex", "message": "Review this change" },
    { "agent": "gemini", "message": "Look for edge cases" }
  ],
  "timeout": 300,
  "project_root_path": "/path/to/project"
}
```

Rules:

- `requests` must contain 1-4 items
- agent names must be unique within a single call
- `timeout` is in seconds
- max timeout is `1800`
- `project_root_path` is optional; if omitted, the current process working directory is used

Response:

```json
{
  "results": [
    {
      "agent": "codex",
      "success": true,
      "response": "..."
    },
    {
      "agent": "gemini",
      "success": false,
      "error": "Request timed out"
    }
  ]
}
```

## Web UI

The web UI is a structured activity view, not a terminal mirror.

Current behavior:

- Left sidebar shows active sessions
- Main pane shows turn cards
- Message text is expanded
- Thought blocks are collapsed by default
- Tool and plan sections are summary-first and expandable
- Permission requests can be approved or denied from the browser
- Follow-up prompts can be sent to an existing session

Sessions are keyed by:

- agent name
- working directory

## Web API

| Endpoint | Method | Purpose |
|---|---|---|
| `/api/status` | `GET` | Current agent/process summary |
| `/api/sessions` | `GET` | Active session list |
| `/api/prompt/{session_id}` | `POST` | Send a follow-up prompt to an existing session |
| `/api/permission/{session_id}` | `POST` | Respond to a pending permission request |
| `/ws` | `GET` | WebSocket event stream |

Example prompt request:

```json
{
  "text": "Continue and fix the failing tests",
  "timeout": 120
}
```

Example permission response:

```json
{
  "id": "permission-id",
  "granted": true
}
```

## Architecture

```text
Claude Code / MCP Client
        |
        | MCP JSON-RPC
        v
  CCGONEXT
    |
    | ask_agents
    v
  SessionManager
    |
    +-- AcpSession(agent, cwd)
            |
            +-- ACP subprocess over stdio
            +-- structured event log
            +-- callback policy enforcement
            +-- timeout / restart handling
    |
    +-- Web server
            |
            +-- REST API
            +-- /ws event stream
            +-- browser activity UI
```

Main modules:

- `src/acp/` - ACP process, protocol, and callback handling
- `src/session/` - session lifecycle and timeout handling
- `src/events.rs` - replayable event log
- `src/mcp/` - MCP server and tool handling
- `src/web/` - browser API and WebSocket streaming

## Development

```bash
cargo build
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## License

This project is dual-licensed.

### Non-Commercial / Personal Use

GNU General Public License v3.0 (`GPL-3.0`)

### Commercial / Workplace Use

Commercial license required.

For commercial licensing inquiries:

- `missdeer@gmail.com`

See:

- [LICENSE](LICENSE)
- [LICENSE-COMMERCIAL](LICENSE-COMMERCIAL)
