# PROJECT KNOWLEDGE BASE

**Generated:** 2026-03-20 22:25:37 CST  
**Commit:** `30ecbfd`  
**Branch:** `main`

## OVERVIEW
ZeroBot 是 Rust 工作区项目：`zerobot-core` 负责 Agent 编排与工具执行，`zerobot-cli` 提供交互入口与 TUI，`zerobot-sdk` 提供嵌入式调用接口。

## STRUCTURE
```text
.
├── crates/
│   ├── zerobot-core/      # 编排核心（会话、工具、provider、hooks、MCP）
│   │   ├── AGENTS.md
│   │   ├── src/AGENTS.md
│   │   └── prompts/AGENTS.md
│   ├── zerobot-cli/       # CLI 与终端 UI
│   │   └── AGENTS.md
│   └── zerobot-sdk/       # 对外 SDK 封装
│       └── AGENTS.md
├── config/                # 配置示例（模板）
└── docs/                  # 静态说明文档
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 主循环/工具调用编排 | `crates/zerobot-core/src/agent.rs` | `Agent::run_turn` 驱动 provider↔tool 循环 |
| 工具注册与执行 | `crates/zerobot-core/src/tool.rs` | 内置工具 + Subagent + Skill + MCP 适配 |
| 会话持久化 | `crates/zerobot-core/src/session.rs` | `SqliteSessionStore` 与消息/审批/todo 存储 |
| 配置加载与优先级 | `crates/zerobot-core/src/config.rs` | `CLI > Managed > Local > Project > User > Defaults` |
| Provider 抽象 | `crates/zerobot-core/src/provider.rs` | OpenAI/Anthropic 统一接口 |
| MCP 接入 | `crates/zerobot-core/src/mcp.rs` | local(stdio)/remote(http) JSON-RPC |
| CLI 入口 | `crates/zerobot-cli/src/main.rs` | 命令解析、会话恢复、provider 选择 |
| TUI 渲染 | `crates/zerobot-cli/src/tui.rs` | 事件循环、流式输出、用户交互覆盖层 |
| SDK 会话调用 | `crates/zerobot-sdk/src/lib.rs` | `ZeroBot` 与 `SessionHandle` |

## CONVENTIONS (PROJECT-SPECIFIC)
- Rust workspace，统一依赖在根 `Cargo.toml` 的 `[workspace.dependencies]`。
- 测试以 **inline `#[cfg(test)] mod tests`** 为主，无 `tests/` 集成测试目录。
- 日志按天写入 `~/.zerobot/logs/YYYY-MM-DD.log`。
- 指令文件就近生效：`AGENTS.md` / `CLAUDE.md` / `CONTEXT.md`。

## ANTI-PATTERNS (THIS PROJECT)
- 未经用户明确要求，不创建文档、不提交代码、不推送变更。
- 不用 `bash` 代替专用工具（read/write/edit/grep/glob 等）。
- 工具被拒绝后不重复同一调用；Hook 返回 `modify` 必须用修改后 payload。
- plan 模式只读：禁止创建/修改/删除/移动文件。

## COMMANDS
```bash
cargo build
cargo test
cargo test -p zerobot-core
cargo run -p zerobot-cli
cargo run -p zerobot-cli -- exec "你好"
```

## NOTES
- `tmp/` 下包含外部镜像/临时内容，不作为项目主代码边界。
- `docs/` 与 `config/` 属静态文档/模板域，不单独拆 AGENTS 子文档。
