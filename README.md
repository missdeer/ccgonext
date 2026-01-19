# CCGO - Claude Code Gateway for Multi-AI Orchestration

[中文文档](README_zh.md)

CCGO is an MCP (Model Context Protocol) server that enables Claude Code to orchestrate multiple AI coding assistants (Codex, Gemini, OpenCode) through a unified interface.

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
cd ccgo
cargo build --release
# Binary at target/release/ccgo (or ccgo.exe on Windows)
```

### Pre-built Binaries

Download from [Releases](https://github.com/missdeer/ccgo/releases).

## Quick Start

### As MCP Server (with Claude Code)

Add to your Claude Code MCP configuration:

```json
{
  "mcpServers": {
    "ccgo": {
      "command": "ccgo",
      "args": ["serve"]
    }
  }
}
```

### Standalone Web UI

```bash
# Start web server only
ccgo web

# Open browser to http://localhost:8765
```

## Usage

```
ccgo [OPTIONS] [COMMAND]

Commands:
  serve   Run as MCP server (stdio mode) with web UI [default]
  web     Run web server only (standalone mode)
  config  Show current configuration

Options:
  -p, --port <PORT>           Web server port [env: CCGO_PORT] [default: 8765]
      --host <HOST>           Web server host [env: CCGO_HOST] [default: 127.0.0.1]
      --input-enabled         Enable web terminal input [env: CCGO_INPUT_ENABLED]
      --auth-token <TOKEN>    Auth token for web API [env: CCGO_AUTH_TOKEN]
      --buffer-size <SIZE>    Output buffer size in bytes [env: CCGO_BUFFER_SIZE] [default: 10485760]
      --timeout <SECONDS>     Default request timeout [env: CCGO_TIMEOUT] [default: 600]
      --codex-cmd <CMD>       Codex command [env: CCGO_CODEX_CMD] [default: codex]
      --gemini-cmd <CMD>      Gemini command [env: CCGO_GEMINI_CMD] [default: gemini]
      --opencode-cmd <CMD>    OpenCode command [env: CCGO_OPENCODE_CMD] [default: opencode]
      --agents <LIST>         Agents to enable (comma-separated) [env: CCGO_AGENTS] [default: codex,gemini,opencode]
  -h, --help                  Print help
  -V, --version               Print version
```

## MCP Tools

CCGO exposes a single MCP tool:

### `ask_agent`

Send a message to an AI agent and wait for response. Agent is auto-started if not running.

```json
{
  "agent_name": "codex",
  "message": "Explain this code",
  "timeout": 300
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
export CCGO_PORT=9000
export CCGO_HOST=0.0.0.0
export CCGO_INPUT_ENABLED=true
export CCGO_AUTH_TOKEN=your-secret-token
export CCGO_AGENTS=codex,gemini
ccgo
```

## WSL2 Network Access

When running in WSL2, to access the web UI from Windows:

```bash
# Bind to all interfaces
ccgo --host 0.0.0.0

# Access via WSL2 IP from Windows
# Get WSL2 IP: ip addr show eth0 | grep "inet "
```

Or set up port forwarding in PowerShell (Admin):

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
│ CCGO MCP Server                                        │    │
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
