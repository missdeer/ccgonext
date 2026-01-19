# CCGO - Claude Code 多 AI 协作网关

[English](README.md)

CCGO 是一个 MCP (Model Context Protocol) 服务器，使 Claude Code 能够通过统一接口编排多个 AI 编程助手（Codex、Gemini、OpenCode）。

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
cd ccgo
cargo build --release
# 二进制文件位于 target/release/ccgo (Windows 为 ccgo.exe)
```

### 预编译二进制

从 [Releases](https://github.com/anthropics/claude-code-bridge/releases) 下载。

## 快速开始

### 作为 MCP 服务器（配合 Claude Code）

添加到 Claude Code MCP 配置：

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

### 独立 Web UI

```bash
# 仅启动 Web 服务器
ccgo web

# 浏览器打开 http://localhost:8765
```

## 使用方法

```
ccgo [选项] [命令]

命令:
  serve   作为 MCP 服务器运行（stdio 模式）并启动 Web UI [默认]
  web     仅运行 Web 服务器（独立模式）
  config  显示当前配置

选项:
  -p, --port <端口>           Web 服务器端口 [环境变量: CCGO_PORT] [默认: 8765]
      --host <地址>           Web 服务器地址 [环境变量: CCGO_HOST] [默认: 127.0.0.1]
      --input-enabled         启用 Web 终端输入 [环境变量: CCGO_INPUT_ENABLED]
      --auth-token <TOKEN>    Web API 认证令牌 [环境变量: CCGO_AUTH_TOKEN]
      --buffer-size <大小>    输出缓冲区大小（字节）[环境变量: CCGO_BUFFER_SIZE] [默认: 10485760]
      --timeout <秒>          默认请求超时 [环境变量: CCGO_TIMEOUT] [默认: 600]
      --codex-cmd <命令>      Codex 启动命令 [环境变量: CCGO_CODEX_CMD] [默认: codex]
      --gemini-cmd <命令>     Gemini 启动命令 [环境变量: CCGO_GEMINI_CMD] [默认: gemini]
      --opencode-cmd <命令>   OpenCode 启动命令 [环境变量: CCGO_OPENCODE_CMD] [默认: opencode]
      --agents <列表>         启用的 Agent（逗号分隔）[环境变量: CCGO_AGENTS] [默认: codex,gemini,opencode]
  -h, --help                  显示帮助
  -V, --version               显示版本
```

## MCP 工具

CCGO 提供一个 MCP 工具：

### `ask_agent`

向 AI Agent 发送消息并等待响应。如果 Agent 未运行，会自动启动。

```json
{
  "agent_name": "codex",
  "message": "解释这段代码",
  "timeout": 300
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
export CCGO_PORT=9000
export CCGO_HOST=0.0.0.0
export CCGO_INPUT_ENABLED=true
export CCGO_AUTH_TOKEN=your-secret-token
export CCGO_AGENTS=codex,gemini
ccgo
```

## WSL2 网络访问

在 WSL2 中运行时，从 Windows 访问 Web UI：

```bash
# 绑定到所有接口
ccgo --host 0.0.0.0

# 从 Windows 通过 WSL2 IP 访问
# 获取 WSL2 IP: ip addr show eth0 | grep "inet "
```

或在 PowerShell（管理员）中设置端口转发：

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
│ CCGO MCP 服务器                                        │    │
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
