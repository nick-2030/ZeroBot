# 项目知识库

**生成时间:** 2026-03-17
**提交:** N/A
**分支:** N/A

## 概述
ZeroBot 是一个基于 Rust 的 AI Agent 系统，提供 CLI（`zerobot-cli`）和核心编排库（`zerobot-core`），以及 SDK（`zerobot-sdk`）。它管理会话、工具和 Agent 交互。

## 结构
```
.
├── crates/
│   ├── zerobot-core/   # 核心逻辑、Agent编排、会话、配置、MCP
│   ├── zerobot-cli/    # 主入口和 TUI
│   └── zerobot-sdk/    # 集成用 SDK
├── config/             # 配置示例
└── target/             # 构建输出
```

## 查找位置
| 任务 | 位置 | 备注 |
|------|------|------|
| Agent 编排 | `crates/zerobot-core/src/agent.rs` | 核心 Agent 定义 |
| 会话状态 | `crates/zerobot-core/src/session.rs` | 对话状态管理 |
| 工具执行 | `crates/zerobot-core/src/tool.rs` | 工具定义和处理 |
| 配置加载 | `crates/zerobot-core/src/config.rs` | 设置加载 |
| CLI 入口 | `crates/zerobot-cli/src/main.rs` | CLI 解析和初始化 |
| TUI 渲染 | `crates/zerobot-cli/src/tui.rs` | 终端界面 |
| MCP 集成 | `crates/zerobot-core/src/mcp.rs` | 模型上下文协议 |

## 约定
- **语言:** Rust 2021 版本。
- **配置:** YAML 格式（`.zerobot/settings.local.yaml`、`config/example.settings.yaml`）。
- **依赖:** 使用 `tokio` 异步 runtime，`tracing` 日志。

## 命令
```bash
cargo build      # 构建工作区
cargo test       # 运行测试
cargo run -p zerobot-cli  # 运行 CLI
```
