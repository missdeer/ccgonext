# CCGONEXT - Claude Code Gateway for Multi-AI Orchestration (Next)

[中文文档](README_zh.md)

CCGONEXT is an MCP (Model Context Protocol) server that enables Claude Code to orchestrate multiple AI coding assistants (Codex, Gemini, OpenCode) through a unified interface.

## Features

- **MCP Protocol Support** - Runs as an MCP server over stdio, seamlessly integrates with Claude Code
- **Multi-Agent Management** - Manage Codex, Gemini, and OpenCode agents from a single interface
- **PTY-based Execution** - Each agent runs in its own pseudo-terminal with full TUI support
- **Web UI Console** - Real-time terminal output via WebSocket, accessible from browser
- **Cross-Platform** - Works on Windows (ConPTY), Linux, and macOS
- **Zero Configuration** - Sensible defaults, all options via CLI args or environment variables
- **Auto-Start Agents** - Agents are automatically started when first message is sent
- **Embedded Static Files** - Single binary deployment, no external files needed

## Installation

### From Source

```bash
cd ccgonext
cargo build --release
# Binary at target/release/ccgonext (or ccgonext.exe on Windows)
```

### Pre-built Binaries

Download from [Releases](https://github.com/missdeer/ccgonext/releases).

## Quick Start

### As MCP Server (with Claude Code)

Add to your Claude Code MCP configuration:

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

### Standalone Web UI

```bash
# Start web server only
ccgonext web

# Or auto-open the UI in your browser
ccgonext web --open-browser
```

## Usage

```
ccgonext [OPTIONS] [COMMAND]

Commands:
  serve   Run as MCP server (stdio mode) with web UI [default]
  web     Run web server only (standalone mode)
  config  Show current configuration

Options:
  -p, --port <PORT>           Web server port [env: CCGONEXT_PORT] [default: 8765]
      --host <HOST>           Web server host [env: CCGONEXT_HOST] [default: 127.0.0.1]
      --port-retry <COUNT>    Retry binding to successive ports if the port is in use [env: CCGONEXT_PORT_RETRY] [default: 0]
      --open-browser          Auto-open the web UI in a browser (web mode only) [env: CCGONEXT_OPEN_BROWSER]
      --show-project-root     Expose local project root path in the web UI/status API [env: CCGONEXT_SHOW_PROJECT_ROOT]
      --windows-enter-delay-ms <MS>  Windows: delay between CR/LF in Enter key sequence [env: CCGONEXT_WINDOWS_ENTER_DELAY_MS] [default: 200]
      --input-enabled         Enable web terminal input [env: CCGONEXT_INPUT_ENABLED]
      --auth-token <TOKEN>    Auth token for web API [env: CCGONEXT_AUTH_TOKEN]
      --buffer-size <SIZE>    Output buffer size in bytes [env: CCGONEXT_BUFFER_SIZE] [default: 10485760]
      --timeout <SECONDS>     Default request timeout [env: CCGONEXT_TIMEOUT] [default: 600]
      --codex-cmd <CMD>       Codex command [env: CCGONEXT_CODEX_CMD] [default: codex]
      --gemini-cmd <CMD>      Gemini command [env: CCGONEXT_GEMINI_CMD] [default: gemini]
      --opencode-cmd <CMD>    OpenCode command [env: CCGONEXT_OPENCODE_CMD] [default: opencode]
      --claudecode-cmd <CMD>  ClaudeCode command [env: CCGONEXT_CLAUDECODE_CMD] [default: claude]
      --agents <LIST>         Agents to enable (comma-separated: codex,gemini,opencode,claudecode) [env: CCGONEXT_AGENTS] [default: codex,gemini,opencode]
      --max-start-retries <N>  Maximum number of retries when agent fails to start [env: CCGONEXT_MAX_START_RETRIES] [default: 3]
      --start-retry-delay <MS> Base delay in milliseconds for exponential backoff between retries [env: CCGONEXT_START_RETRY_DELAY] [default: 1000]
      --log-file <PATH>       Log file path (optional, if not set logs only go to stderr) [env: CCGONEXT_LOG_FILE]
      --log-dir <PATH>        Log directory for rotating logs [env: CCGONEXT_LOG_DIR]
  -h, --help                  Print help
  -V, --version               Print version
```

## MCP Tools

CCGONEXT exposes a single MCP tool:

### `ask_agents`

Send messages to AI agents in parallel and wait for responses. Agents are auto-started if not running.

```json
{
  "requests": [
    {"agent": "codex", "message": "Review this code"},
    {"agent": "gemini", "message": "Suggest improvements"}
  ],
  "timeout": 300
}
```

**Parameters:**
- `requests`: Array of 1-4 agent requests
  - `agent`: "codex" | "gemini" | "opencode" | "claudecode"
  - `message`: Prompt to send
- `timeout`: Optional seconds (default: 600, max: 1800)

**Response:**
```json
{
  "results": [
    {"agent": "codex", "success": true, "response": "..."},
    {"agent": "gemini", "success": true, "response": "..."}
  ]
}
```

## Web UI

Access the web interface at `http://localhost:8765`:

| URL | Description |
|-----|-------------|
| `/` | Overview - 2x2 grid showing all agents |
| `/codex` | Full-screen Codex terminal |
| `/gemini` | Full-screen Gemini terminal |
| `/opencode` | Full-screen OpenCode terminal |
| `/claudecode` | Full-screen ClaudeCode terminal |

### Web API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/status` | GET | Get status of all agents |
| `/ws/:agent` | WebSocket | Real-time terminal I/O |

## Environment Variables

All CLI options can be set via environment variables:

```bash
export CCGONEXT_PORT=9000
export CCGONEXT_HOST=0.0.0.0
export CCGONEXT_PORT_RETRY=20
export CCGONEXT_INPUT_ENABLED=true
export CCGONEXT_AUTH_TOKEN=your-secret-token
export CCGONEXT_OPEN_BROWSER=true
export CCGONEXT_SHOW_PROJECT_ROOT=true
export CCGONEXT_WINDOWS_ENTER_DELAY_MS=200
export CCGONEXT_AGENTS=codex,gemini
ccgonext web
```

## WSL2 Network Access

When running in WSL2, to access the web UI from Windows:

```bash
# Bind to all interfaces inside WSL (so Windows can reach it)
ccgonext web --host 0.0.0.0
```

The web UI enforces a localhost-only Origin check by default, so accessing via the WSL IP won't work. Use Windows port forwarding and access via `http://localhost:8765`.

Set up port forwarding in PowerShell (Admin):

```powershell
netsh interface portproxy add v4tov4 listenport=8765 listenaddress=0.0.0.0 connectport=8765 connectaddress=$(wsl hostname -I)
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│ Claude Code                                                 │
│   └── MCP Client ──────────────────────────────────────┐    │
└────────────────────────────────────────────────────────│────┘
                                                         │
┌────────────────────────────────────────────────────────│────┐
│ CCGONEXT MCP Server                                        │    │
│   ┌──────────────┐    ┌─────────────────────────────┐  │    │
│   │ MCP Handler  │◄───│ stdio (JSON-RPC)            │◄─┘    │
│   └──────┬───────┘    └─────────────────────────────┘       │
│          │                                                  │
│   ┌──────▼───────┐    ┌─────────────────────────────┐       │
│   │ Session Mgr  │───►│ PTY Manager                 │       │
│   └──────────────┘    │  ├── codex (ConPTY/TTY)     │       │
│          │            │  ├── gemini (ConPTY/TTY)    │       │
│          │            │  └── opencode (ConPTY/TTY)  │       │
│   ┌──────▼───────┐    └─────────────────────────────┘       │
│   │ Log Provider │                                          │
│   │  ├── Codex   │    ┌─────────────────────────────┐       │
│   │  ├── Gemini  │    │ Web Server (axum)           │       │
│   │  └── OpenCode│    │  ├── REST API               │       │
│   └──────────────┘    │  ├── WebSocket              │       │
│                       │  └── Static Files           │       │
└───────────────────────┴─────────────────────────────────────┘
```

## Building

```bash
# Debug build
cargo build

# Release build (optimized, stripped)
cargo build --release

# Run tests
cargo test

# Run clippy
cargo clippy --all-targets --all-features -- -D warnings
```

## License

This project is dual-licensed:

### Non-Commercial / Personal Use
**GNU General Public License v3.0 (GPL-3.0)**

Free for:
- Personal projects and hobby use
- Educational purposes and academic research
- Open source projects
- Evaluation and testing

### Commercial / Workplace Use
**Commercial License Required**

A commercial license is required if you use this software:
- In a workplace environment (for-profit or non-profit)
- As part of commercial products or services
- For internal business tools or operations
- For consulting or client work
- As a SaaS or cloud service

For commercial licensing inquiries, please contact: **missdeer@gmail.com**

See [LICENSE](LICENSE) and [LICENSE-COMMERCIAL](LICENSE-COMMERCIAL) for details.
