# CCGONEXT - Claude Code 多 AI 协作网关 (Next)

[English](README.md)

CCGONEXT 是一个 MCP (Model Context Protocol) 服务器，使 Claude Code 能够通过统一接口编排多个 AI 编程助手（Codex、Gemini、OpenCode）。

## 功能特性

- **MCP 协议支持** - 作为 MCP 服务器通过 stdio 运行，与 Claude Code 无缝集成
- **多 Agent 管理** - 在单一界面管理 Codex、Gemini 和 OpenCode
- **基于 PTY 执行** - 每个 Agent 运行在独立的伪终端中，完整支持 TUI
- **Web UI 控制台** - 通过 WebSocket 实时输出终端内容，可在浏览器中访问
- **跨平台支持** - 支持 Windows (ConPTY)、Linux 和 macOS
- **零配置** - 合理的默认值，所有选项通过命令行参数或环境变量配置
- **自动启动 Agent** - 首次发送消息时自动启动 Agent
- **内嵌静态文件** - 单二进制部署，无需额外文件

## 安装

### 从源码编译

```bash
cd ccgonext
cargo build --release
# 二进制文件位于 target/release/ccgonext (Windows 为 ccgonext.exe)
```

### 预编译二进制

从 [Releases](https://github.com/anthropics/claude-code-bridge/releases) 下载。

## 快速开始

### 作为 MCP 服务器（配合 Claude Code）

添加到 Claude Code MCP 配置：

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

### 独立 Web UI

```bash
# 仅启动 Web 服务器
ccgonext web

# 或自动在浏览器中打开 UI
ccgonext web --open-browser
```

## 使用方法

```
ccgonext [选项] [命令]

命令:
  serve   作为 MCP 服务器运行（stdio 模式）并启动 Web UI [默认]
  web     仅运行 Web 服务器（独立模式）
  config  显示当前配置

选项:
  -p, --port <端口>           Web 服务器端口 [环境变量: CCGONEXT_PORT] [默认: 8765]
      --host <地址>           Web 服务器地址 [环境变量: CCGONEXT_HOST] [默认: 127.0.0.1]
      --port-retry <次数>     端口被占用时，递增端口重试绑定 [环境变量: CCGONEXT_PORT_RETRY] [默认: 0]
      --open-browser          自动在浏览器中打开 Web UI（仅 web 模式）[环境变量: CCGONEXT_OPEN_BROWSER]
      --show-project-root     在 Web UI/status API 中显示本地项目根目录路径 [环境变量: CCGONEXT_SHOW_PROJECT_ROOT]
      --windows-enter-delay-ms <毫秒>  Windows: Enter 键序列 CR/LF 之间的延迟 [环境变量: CCGONEXT_WINDOWS_ENTER_DELAY_MS] [默认: 200]
      --input-enabled         启用 Web 终端输入 [环境变量: CCGONEXT_INPUT_ENABLED]
      --auth-token <TOKEN>    Web API 认证令牌 [环境变量: CCGONEXT_AUTH_TOKEN]
      --buffer-size <大小>    输出缓冲区大小（字节）[环境变量: CCGONEXT_BUFFER_SIZE] [默认: 10485760]
      --timeout <秒>          默认请求超时 [环境变量: CCGONEXT_TIMEOUT] [默认: 600]
      --codex-cmd <命令>      Codex 启动命令 [环境变量: CCGONEXT_CODEX_CMD] [默认: codex]
      --gemini-cmd <命令>     Gemini 启动命令 [环境变量: CCGONEXT_GEMINI_CMD] [默认: gemini]
      --opencode-cmd <命令>   OpenCode 启动命令 [环境变量: CCGONEXT_OPENCODE_CMD] [默认: opencode]
      --claudecode-cmd <命令> ClaudeCode 启动命令 [环境变量: CCGONEXT_CLAUDECODE_CMD] [默认: claude]
      --agents <列表>         启用的 Agent（逗号分隔: codex,gemini,opencode,claudecode）[环境变量: CCGONEXT_AGENTS] [默认: codex,gemini,opencode]
      --max-start-retries <次数>  Agent 启动失败时的最大重试次数 [环境变量: CCGONEXT_MAX_START_RETRIES] [默认: 3]
      --start-retry-delay <毫秒>  启动重试的基础延迟（指数退避）[环境变量: CCGONEXT_START_RETRY_DELAY] [默认: 1000]
      --log-file <路径>       日志文件路径（可选，未设置则仅输出到 stderr）[环境变量: CCGONEXT_LOG_FILE]
      --log-dir <路径>        轮转日志目录 [环境变量: CCGONEXT_LOG_DIR]
  -h, --help                  显示帮助
  -V, --version               显示版本
```

## MCP 工具

CCGONEXT 提供一个 MCP 工具：

### `ask_agents`

向 AI Agent 并行发送消息并等待响应。如果 Agent 未运行，会自动启动。

```json
{
  "requests": [
    {"agent": "codex", "message": "审查这段代码"},
    {"agent": "gemini", "message": "提出改进建议"}
  ],
  "timeout": 300
}
```

**参数：**
- `requests`：1-4 个 Agent 请求的数组
  - `agent`："codex" | "gemini" | "opencode" | "claudecode"
  - `message`：要发送的提示
- `timeout`：可选，超时秒数（默认：600，最大：1800）

**响应：**
```json
{
  "results": [
    {"agent": "codex", "success": true, "response": "..."},
    {"agent": "gemini", "success": true, "response": "..."}
  ]
}
```

## Web UI

访问 `http://localhost:8765`：

| URL | 说明 |
|-----|------|
| `/` | 总览 - 2x2 网格显示所有 Agent |
| `/codex` | Codex 全屏终端 |
| `/gemini` | Gemini 全屏终端 |
| `/opencode` | OpenCode 全屏终端 |
| `/claudecode` | ClaudeCode 全屏终端 |

### Web API

| 端点 | 方法 | 说明 |
|------|------|------|
| `/api/status` | GET | 获取所有 Agent 状态 |
| `/ws/:agent` | WebSocket | 实时终端 I/O |

## 环境变量

所有命令行选项都可以通过环境变量设置：

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

## WSL2 网络访问

在 WSL2 中运行时，从 Windows 访问 Web UI：

```bash
# 在 WSL 中绑定到所有接口（便于 Windows 访问）
ccgonext web --host 0.0.0.0
```

Web UI 默认只允许 localhost Origin，因此不能直接通过 WSL2 IP 访问；请使用 Windows 端口转发，并通过 `http://localhost:8765` 访问。

在 PowerShell（管理员）中设置端口转发：

```powershell
netsh interface portproxy add v4tov4 listenport=8765 listenaddress=0.0.0.0 connectport=8765 connectaddress=$(wsl hostname -I)
```

## 架构

```
┌─────────────────────────────────────────────────────────────┐
│ Claude Code                                                 │
│   └── MCP 客户端 ──────────────────────────────────────┐    │
└────────────────────────────────────────────────────────│────┘
                                                         │
┌────────────────────────────────────────────────────────│────┐
│ CCGONEXT MCP 服务器                                        │    │
│   ┌──────────────┐    ┌─────────────────────────────┐  │    │
│   │ MCP 处理器   │◄───│ stdio (JSON-RPC)            │◄─┘    │
│   └──────┬───────┘    └─────────────────────────────┘       │
│          │                                                  │
│   ┌──────▼───────┐    ┌─────────────────────────────┐       │
│   │ 会话管理器   │───►│ PTY 管理器                  │       │
│   └──────────────┘    │  ├── codex (ConPTY/TTY)     │       │
│          │            │  ├── gemini (ConPTY/TTY)    │       │
│          │            │  └── opencode (ConPTY/TTY)  │       │
│   ┌──────▼───────┐    └─────────────────────────────┘       │
│   │ 日志提供器   │                                          │
│   │  ├── Codex   │    ┌─────────────────────────────┐       │
│   │  ├── Gemini  │    │ Web 服务器 (axum)           │       │
│   │  └── OpenCode│    │  ├── REST API               │       │
│   └──────────────┘    │  ├── WebSocket              │       │
│                       │  └── 静态文件               │       │
└───────────────────────┴─────────────────────────────────────┘
```

## 构建

```bash
# 调试构建
cargo build

# 发布构建（优化、裁剪）
cargo build --release

# 运行测试
cargo test

# 运行 clippy
cargo clippy --all-targets --all-features -- -D warnings
```

## 许可证

本项目采用双许可模式：

### 非商业 / 个人使用
**GNU 通用公共许可证 v3.0 (GPL-3.0)**

以下情况免费使用：
- 个人项目和业余爱好
- 教育目的和学术研究
- 开源项目
- 评估和测试

### 商业 / 工作场所使用
**需要商业许可证**

以下情况需要商业许可证：
- 在工作环境中使用（营利或非营利组织）
- 作为商业产品或服务的一部分
- 用于内部业务工具或运营
- 用于咨询或客户项目
- 作为 SaaS 或云服务

商业许可咨询请联系：**missdeer@gmail.com**

详情请参阅 [LICENSE](LICENSE) 和 [LICENSE-COMMERCIAL](LICENSE-COMMERCIAL)。
