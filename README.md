# ZeroBot

一个基于 Rust 的 AI Agent 系统，提供 CLI 和核心编排库，支持多 Agent 协作、工具调用和 MCP（Model Context Protocol）集成。

## 特性

- 🤖 **多 Agent 编排** - 支持定义和管理多个 Agent，实现复杂任务分解
- 🛠️ **工具系统** - 内置工具注册和执行机制，支持自定义工具扩展
- 💬 **会话管理** - 基于 SQLite 的持久化会话存储，支持会话历史回溯
- 🔌 **MCP 集成** - 支持 Model Context Protocol，与外部服务无缝集成
- 🎨 **TUI 界面** - 提供友好的终端用户界面
- ⚙️ **灵活配置** - 支持多层级配置（项目级/用户级/系统级）

## 快速开始

### 环境要求

- Rust 2021 版本或更高
- SQLite 3.x

### 安装

```bash
# 构建项目
cargo build

# 运行 CLI
cargo run -p zerobot-cli
```

### 基本使用

```bash
# 执行单个提示
cargo run -p zerobot-cli exec "帮我写一个函数"

# 进入交互模式
cargo run -p zerobot-cli

# 会话管理
cargo run -p zerobot-cli session new "我的任务"
cargo run -p zerobot-cli session list
cargo run -p zerobot-cli session show <session-id>
```

## 项目结构

```
.
├── crates/
│   ├── zerobot-core/    # 核心逻辑、Agent 编排、会话、配置、MCP
│   ├── zerobot-cli/     # CLI 入口和 TUI
│   └── zerobot-sdk/     # 集成用 SDK
├── config/              # 配置示例
└── target/              # 构建输出
```

## 核心模块

| 模块 | 位置 | 说明 |
|------|------|------|
| Agent 编排 | `crates/zerobot-core/src/agent.rs` | 核心 Agent 定义 |
| 会话管理 | `crates/zerobot-core/src/session.rs` | 对话状态管理 |
| 工具执行 | `crates/zerobot-core/src/tool.rs` | 工具定义和处理 |
| 配置加载 | `crates/zerobot-core/src/config.rs` | 设置加载 |
| CLI 入口 | `crates/zerobot-cli/src/main.rs` | CLI 解析和初始化 |
| TUI 渲染 | `crates/zerobot-cli/src/tui.rs` | 终端界面 |
| MCP 集成 | `crates/zerobot-core/src/mcp.rs` | 模型上下文协议 |

## 配置

配置文件使用 YAML 格式：

- 项目配置：`.zerobot/settings.local.yaml`
- 示例配置：`config/example.settings.yaml`

## 开发

```bash
# 构建工作区
cargo build

# 运行测试
cargo test

# 运行 CLI
cargo run -p zerobot-cli
```

## 文档

- [使用指南](docs/USAGE.md) - 详细的使用说明
- [架构文档](docs/ARCHITECTURE.md) - 技术架构和设计
- [贡献指南](docs/CONTRIBUTING.md) - 参与开发

## 许可证

[待添加]

## 相关链接

- [AGENTS.md](AGENTS.md) - AI 助手知识库（供 AI 助手阅读的项目信息）
