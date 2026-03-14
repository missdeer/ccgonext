# CCGONEXT

[English](README.md)

CCGONEXT 是一个 MCP 服务器，用来让 Claude Code 通过单一 MCP 工具编排多个代码 agent，并在浏览器里提供结构化的 ACP 活动视图。

## 当前能力

- 通过 stdio 运行 MCP 服务
- 通过 ACP (Agent Client Protocol) over stdio 与 agent 通信，不再依赖 PTY 镜像
- 支持 `codex`、`gemini`、`opencode`、`claudecode`
- 为每个 agent 和工作目录维护独立 session
- 向 Web UI 推送结构化事件：消息、thought、plan、tool call、permission、状态变化
- 浏览器可以对已有 session 发送 follow-up prompt
- 支持按 agent 配置 ACP callback policy
- 静态文件内嵌在二进制中，部署简单

## 支持的 Agent 命令

默认 ACP 命令：

- `codex` -> `codex-acp`
- `gemini` -> `gemini --acp`
- `opencode` -> `opencode acp`
- `claudecode` -> `claude-agent-acp`

这些都可以通过 CLI 或环境变量覆盖。

## 安装

### 从源码构建

```bash
cargo build --release
```

输出文件：

- Linux/macOS: `target/release/ccgonext`
- Windows: `target/release/ccgonext.exe`

### 预编译版本

从 [Releases](https://github.com/missdeer/ccgonext/releases) 下载。

## 快速开始

### 配合 Claude Code 使用

在 Claude Code 的 MCP 配置里加入：

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

### 启动 Web UI

```bash
ccgonext web --open-browser
```

### 查看当前生效配置

```bash
ccgonext config
```

## 命令

```text
ccgonext [OPTIONS] [COMMAND]

Commands:
  serve   作为 MCP 服务器运行（stdio 模式）并启动 Web UI
  web     仅运行 Web 服务器（独立模式）
  config  显示当前配置
```

关键参数：

| 参数 | 环境变量 | 默认值 |
|---|---|---|
| `--port` | `CCGONEXT_PORT` | `8765` |
| `--host` | `CCGONEXT_HOST` | `127.0.0.1` |
| `--port-retry` | `CCGONEXT_PORT_RETRY` | `0` |
| `--open-browser` | `CCGONEXT_OPEN_BROWSER` | `false` |
| `--auth-token` | `CCGONEXT_AUTH_TOKEN` | 未设置 |
| `--timeout` | `CCGONEXT_TIMEOUT` | `600` |
| `--idle-timeout` | `CCGONEXT_IDLE_TIMEOUT` | `900` |
| `--agents` | `CCGONEXT_AGENTS` | `codex,gemini,opencode` |
| `--codex-cmd` | `CCGONEXT_CODEX_CMD` | `codex-acp` |
| `--gemini-cmd` | `CCGONEXT_GEMINI_CMD` | `gemini` |
| `--opencode-cmd` | `CCGONEXT_OPENCODE_CMD` | `opencode` |
| `--claudecode-cmd` | `CCGONEXT_CLAUDECODE_CMD` | `claude-agent-acp` |
| `--callback-policy` | `CCGONEXT_CALLBACK_POLICY` | `auto-approve` |
| `--codex-callback-policy` | `CCGONEXT_CODEX_CALLBACK_POLICY` | 继承全局 |
| `--gemini-callback-policy` | `CCGONEXT_GEMINI_CALLBACK_POLICY` | 继承全局 |
| `--opencode-callback-policy` | `CCGONEXT_OPENCODE_CALLBACK_POLICY` | 继承全局 |
| `--claudecode-callback-policy` | `CCGONEXT_CLAUDECODE_CALLBACK_POLICY` | 继承全局 |
| `--log-file` | `CCGONEXT_LOG_FILE` | 未设置 |
| `--log-dir` | `CCGONEXT_LOG_DIR` | 未设置 |

完整帮助：

```bash
ccgonext --help
```

## Callback 策略

ACP agent 可以通过 callback 请求权限、读写文件和执行终端命令。CCGONEXT 目前支持四种策略：

- `deny-all`
- `read-only`
- `ask`
- `auto-approve`

当前默认值：

- 所有已启用 agent 默认 `auto-approve`

示例：

```bash
# 默认行为
ccgonext serve

# 为所有已启用 agent 统一设置策略
ccgonext serve --callback-policy read-only

# 单独覆盖某个 agent
ccgonext serve --callback-policy read-only --codex-callback-policy auto-approve
```

对应环境变量：

```bash
export CCGONEXT_CALLBACK_POLICY=read-only
export CCGONEXT_CODEX_CALLBACK_POLICY=auto-approve
export CCGONEXT_GEMINI_CALLBACK_POLICY=ask
```

`auto-approve` 最省事，但它也意味着 agent 可以自动批准文件写入、终端执行等 ACP 动作。如果你的环境不适合这种权限模型，就把策略调严。

## MCP 工具

CCGONEXT 当前暴露一个 MCP 工具：`ask_agents`。

### `ask_agents`

向 1-4 个 agent 并行发送 prompt。

请求示例：

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

规则：

- `requests` 必须有 1-4 项
- 同一次调用里 agent 名称不能重复
- `timeout` 单位是秒
- 最大超时是 `1800`
- `project_root_path` 可选；不传就使用当前进程工作目录

响应示例：

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

当前 Web UI 是结构化活动视图，不是终端镜像。

主要行为：

- 左侧栏显示 active sessions
- 主区域显示 turn cards
- 正文直接展开
- thought 默认折叠
- tool 和 plan 默认显示摘要，可展开查看细节
- permission request 可以直接在浏览器里 approve/deny
- 可以对已有 session 发送 follow-up prompt

session 维度：

- agent 名称
- 工作目录

## Web API

| 路径 | 方法 | 作用 |
|---|---|---|
| `/api/status` | `GET` | 当前 agent / process 摘要 |
| `/api/sessions` | `GET` | 活跃 session 列表 |
| `/api/prompt/{session_id}` | `POST` | 向已有 session 发送 follow-up prompt |
| `/api/permission/{session_id}` | `POST` | 响应待处理 permission request |
| `/ws` | `GET` | WebSocket 事件流 |

prompt 请求示例：

```json
{
  "text": "Continue and fix the failing tests",
  "timeout": 120
}
```

permission 响应示例：

```json
{
  "id": "permission-id",
  "granted": true
}
```

## 架构

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

主要模块：

- `src/acp/` - ACP 进程、协议、callback 处理
- `src/session/` - session 生命周期和超时逻辑
- `src/events.rs` - 可重放事件日志
- `src/mcp/` - MCP 服务器和工具处理
- `src/web/` - 浏览器 API 和 WebSocket 事件流

## 开发

```bash
cargo build
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## 许可证

本项目采用双许可证。

### 非商业 / 个人使用

GNU General Public License v3.0 (`GPL-3.0`)

### 商业 / 工作环境使用

需要商业许可证。

商业授权联系：

- `missdeer@gmail.com`

详见：

- [LICENSE](LICENSE)
- [LICENSE-COMMERCIAL](LICENSE-COMMERCIAL)
