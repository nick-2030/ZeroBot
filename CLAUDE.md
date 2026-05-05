# CLAUDE.md

本文件为 Claude Code (claude.ai/code) 在此仓库中工作时提供指引。

## 项目概述

ZeroBot 是一个基于 Rust 的 AI Agent 系统，提供 CLI/TUI 和 SDK 接口。支持多智能体编排、工具调用、MCP（Model Context Protocol）集成、持久化会话、网关模式、插件/技能系统。

## 构建与测试命令

```bash
cargo build                          # 构建整个工作区
cargo test                           # 运行所有测试
cargo test -p zerobot-core           # 仅运行核心库测试
cargo test -p zerobot-cli            # 仅运行 CLI 测试
cargo clippy                         # 代码检查
cargo run -p zerobot-cli             # 交互式 TUI 模式
cargo run -p zerobot-cli -- exec "你好"   # 一次性执行
cargo run -p zerobot-cli -- gateway         # 网关守护进程模式
cargo run -p zerobot-cli -- acp             # Agent Client Protocol 服务器
cargo run -p zerobot-cli -- session list    # 列出会话
cargo run -p zerobot-cli -- config show     # 查看配置
```

测试使用内联 `#[cfg(test)] mod tests` 模块（无 `tests/` 目录）。开发依赖：`httpmock` 用于 HTTP 模拟，`tempfile` 用于临时目录，`pretty_assertions` 用于差异对比。

## 工作区架构

三个 crate，依赖统一管理于根 `Cargo.toml` 的 `[workspace.dependencies]`：

**zerobot-core** — 核心库，包含所有编排逻辑：
- `agent.rs` — `Agent::run_turn()` 驱动 provider↔tool 执行循环
- `tool.rs` — `ToolRegistry` 管理内置工具（read, write, edit, bash, glob, grep）+ Subagent/Skill/MCP 适配器
- `session.rs` — `SqliteSessionStore` 持久化会话、消息、工具调用、审批、待办
- `config.rs` — 6 层配置优先级：CLI > Managed > Local > Project > User > Defaults
- `provider.rs` — `Provider` trait，含 `OpenAIProvider` 和 `AnthropicProvider`
- `mcp.rs` — MCP 客户端（stdio 本地 / HTTP 远程 JSON-RPC）
- `hooks.rs` — `HookManager`，20+ 钩子事件（Allow/Deny/Modify 决策）
- `context.rs` — `ContextManager` 系统提示词组装、历史裁剪、上下文压缩
- `gateway.rs` — `GatewayRuntime` 长运行守护进程事件循环
- `swarm/` — 多智能体协作，含 `TeammateBackend` trait
- `skills.rs` — 技能发现，支持 `.claude/skills`、`.agents/skills`、`.zerobot/skills/`、远程 URL
- `memory.rs` — 记忆存储，含提示词注入检测
- `plugin.rs` — JSON-RPC 插件系统
- `kanban.rs` — 看板任务管理
- `agent_dispatch.rs` — 多智能体分发，支持隔离模式

**zerobot-cli** — CLI 二进制与 TUI：
- `main.rs` — 基于 clap 的 CLI（exec、session、config、gateway、cron、acp 子命令）
- `tui/` — 基于 ratatui 的终端 UI，含 14 个组件、按键绑定系统、Markdown 渲染

**zerobot-sdk** — 可嵌入 SDK：
- `lib.rs` — `ZeroBot` 客户端，提供 `query()`/`query_stream()`、`SessionHandle` 管理会话生命周期

## 核心架构模式

- `Agent::run_turn()` 中的智能体循环交替执行 LLM 调用和工具执行，直到完成或达到最大步数（默认 100）。
- 工具调用可根据 provider 响应并行或串行执行。
- 上下文压缩在上下文窗口接近限制时自动触发，将历史摘要为压缩锚点。
- 系统提示词由 `prompts/system/*.md`（identity、tools、style 等）和 `prompts/modes/*.md`（execute、plan、review、coordinator）组装而成。
- 钩子在 20+ 生命周期节点触发，可允许、拒绝或修改载荷。
- 多智能体分发可配置隔离模式（进程内或独立会话）生成子智能体。

## 配置

优先级（从高到低）：CLI `--set` > managed `/etc/zerobot/` > local `.zerobot/settings.local.yaml` > project `.zerobot/settings.yaml` > user `~/.zerobot/settings.yaml` > 默认值。

运行时路径：会话数据库 `~/.zerobot/state/workspaces/{workspace}/zerobot.db`，日志 `~/.zerobot/logs/YYYY-MM-DD.log`。

环境变量：`OPENAI_API_KEY` 或 `ANTHROPIC_API_KEY`（或通过 settings YAML 配置）。

## 项目约定

- Rust 2021 edition，`resolver = "2"`，Apache-2.0 许可证。
- 注释、文档、日志消息使用中文；代码标识符使用英文。
- 未经明确要求，不创建文档、不提交代码、不推送变更。
- `tmp/` 包含外部参考材料，非项目源代码。
- 各级 `AGENTS.md` 文件为 AI 助手提供项目上下文。
