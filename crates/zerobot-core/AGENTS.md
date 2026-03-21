# zerobot-core

## OVERVIEW
核心编排库：把 provider、tools、session、hooks、skills、instructions 串成一个可迭代回合循环。

## STRUCTURE
```text
.
├── src/            # 运行时代码（含核心状态机）
├── prompts/        # 系统提示词分片（system + modes）
├── AGENTS.md
├── src/AGENTS.md
└── prompts/AGENTS.md
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| Agent 回合循环 | `src/agent.rs` | 包含上下文构建、provider 流式消费、tool 调用 |
| 工具系统 | `src/tool.rs` | `ToolRegistry` + 内置工具 + Subagent/Skill/MCP 入口 |
| 会话与存储 | `src/session.rs` | `SessionStore` trait 与 `SqliteSessionStore` |
| 配置与策略 | `src/config.rs` | 多层配置合并与工具审批策略 |
| Provider 实现 | `src/provider.rs` | OpenAI/Anthropic 请求与流事件适配 |
| Hook 执行链 | `src/hooks.rs` | event matcher、allow/deny/modify 协议 |
| Prompt 组装 | `src/prompt.rs` + `prompts/` | 模块化拼接系统提示词 |
| MCP 集成 | `src/mcp.rs` | 本地/远程 MCP 客户端管理 |

## CONVENTIONS
- 对外 API 统一由 `src/lib.rs` re-export。
- `Settings`/`LoadedConfig` 作为跨模块配置载体，避免散落读取配置文件。
- Hook 事件是软约束链：失败记录 warning，不阻塞主流程；明确 deny 才中断。
- Tool 输出统一走 `render_tool_output`，截断规则由 `tools.output` 配置控制。

## ANTI-PATTERNS
- 在 `agent.rs` 里绕过 Hook 或审批检查直接执行工具。
- 在 `tool.rs` 新增写文件能力时跳过 “read-before-write” 校验逻辑。
- 在 `session.rs` 改 schema 但不同时维护初始化 SQL 与读取逻辑。

## TEST PATTERNS
- 以 inline `#[cfg(test)] mod tests` 为主。
- IO/配置用 `tempfile::TempDir`；网络用 `httpmock`；异步用 `#[tokio::test]`。

## COMMANDS
```bash
cargo test -p zerobot-core
cargo test -p zerobot-core context::tests
cargo test -p zerobot-core -- --nocapture
```
