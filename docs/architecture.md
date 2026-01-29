# CCGONEXT Project Architecture

## 1. Overview

CCGONEXT is a multi-AI collaboration bridge that implements the Model Context Protocol (MCP). It acts as a server that manages multiple AI agents (Codex, Gemini, OpenCode, ClaudeCode) running as CLI processes. It provides a unified interface for clients to interact with these agents, handling process lifecycle, terminal emulation (PTY), and output parsing.

The system is built in Rust using `tokio` for asynchronous execution and `portable-pty` for terminal management.

## 2. System Architecture

The project is organized into several modular components:

```mermaid
graph TD
    Client[MCP Client] <--> |JSON-RPC (stdio)| MCP[MCP Server]
    WebClient[Web Browser] <--> |HTTP/WebSocket| Web[Web Server]
    
    MCP --> SessionMgr[Session Manager]
    Web --> SessionMgr
    
    SessionMgr --> |Manages| Session[Agent Session]
    
    Session --> |Uses| Agent[Agent Adapter]
    Session --> |Controls| PTY[PTY Manager]
    Session --> |Reads| Log[Log Provider]
    Session --> |Tracks| State[State Machine]
    
    PTY --> |Spawns| Process[External CLI Process]
    Log --> |Watches| Files[Log Files]
    Process --> |Output| PTY
    Process --> |Writes| Files
```

## 3. Core Modules

### 3.1. MCP Server (`src/mcp/`)
- **Role**: Entry point for MCP clients (e.g., IDE extensions).
- **Transport**: Supports both standard Line-Delimited JSON (JSONL) and LSP-style (Content-Length header) transports over Stdio.
- **Tools**: Exposes tools like `ask_agents` which allows parallel querying of multiple agents.
- **Protocol**: Implements JSON-RPC 2.0 request/response handling.

### 3.2. Session Management (`src/session/`)
- **SessionManager**: Central registry for all active agent sessions. Handles concurrent access and shutdown.
- **AgentSession**: Represents a single agent instance.
  - Manages the request queue.
  - Coordinates PTY writing and Reply detection.
  - Implements concurrency control (locking) for thread safety.
  - Handles retries and timeouts (`TimeoutConfig`).

### 3.3. PTY Layer (`src/pty/`)
- **PtyManager**: Abstraction over `portable-pty`.
- **PtyHandle**: Manages a single PTY process.
  - **Output Buffering**: Maintains a circular buffer of terminal output.
  - **Windows Compatibility**: Handles platform specific quirks (e.g., `cmd.exe` wrapping, Enter key delays).
  - **Terminal Queries**: Automatically responds to terminal query sequences (e.g., CPR, DSR) to prevent blocking.

### 3.4. Log Provider (`src/log_provider/`)
- **Purpose**: Detects when an agent has finished generating a response since many CLI agents do not have standard stdout delimiting.
- **Abstraction**: `LogProvider` trait.
- **Implementations**:
  - `CodexLogProvider`: Parsers `.jsonl` session files.
  - `GeminiLogProvider`: Parsers JSON chat history, handles file rotation and project hashing.
  - `OpenCodeLogProvider`: Monitors storage directories for updated session files.
  - `NullLogProvider`: Used for agents that don't output to logs.
- **Features**: Supports file watching (debounced) and polling fallbacks.

### 3.5. Agent Adapters (`src/agent/`)
- **Agent Trait**: Standardizes interaction with different CLI tools.
- **GenericAgent**: Configurable implementation for standard agents.
- **ClaudeCodeAgent**: Specialized implementation that uses PTY output parsing (ANSI stripping, sentinel detection) instead of external log files.

### 3.6. State Machine (`src/state/`)
- **AgentState**: Enum representing lifecycle states (`Stopped`, `Starting`, `Idle`, `Busy`, `Dead`, etc.).
- **StateMachine**: Pure function determining transitions and side effects based on events.
- **Transitions**: strict rules for state changes (e.g., `STARTING` -> `IDLE` on ReadyDetected).

### 3.7. Web Server (`src/web/`)
- **Framework**: Built with `axum`.
- **Features**:
  - **Status API**: View running agents and their states.
  - **Control API**: Restart agents.
  - **WebSocket**: Real-time streaming of PTY output to web clients.
  - **Static Files**: Serves embedded UI assets.
  - **Auth**: Token-based authentication and Origin validation.

## 4. Key Workflows

### 4.1. Starting an Agent
1. `SessionManager` triggers start.
2. `AgentSession` requests PTY creation.
3. `PtyManager` spawns the process (wrapping command for OS compatibility).
4. `AgentSession` monitors PTY output for a "Ready Pattern" (regex).
5. State transitions from `Starting` -> `Idle`.

### 4.2. "Ask Agents" Request
1. MCP receives `ask_agents` tool call.
2. `SessionManager` routes requests to appropriate `AgentSession`s.
3. `AgentSession`:
   - Checks state (must be `Idle`).
   - Injects a "Sentinel" (unique ID) into the message.
   - Writes message to PTY.
   - Spawns a "Reply Detection" task.
4. **Reply Detection**:
   - Watches log files (or PTY output for ClaudeCode).
   - Waits for "Done Marker" or stability (no changes for X seconds).
   - Returns extracted content.
5. Response returned to MCP client.

## 5. Configuration
Configuration is loaded from environment variables and CLI arguments (`src/config/mod.rs`), managing:
- Server settings (host, port).
- Agent definitions (commands, patterns).
- Timeouts and retry policies.
- Web interface settings.
