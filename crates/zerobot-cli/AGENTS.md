# zerobot-cli

## OVERVIEW
命令行与 TUI 前端：解析命令、加载配置、初始化会话与 provider，并把交互流转给 `zerobot-core`。

## STRUCTURE
```text
.
├── src/
│   ├── main.rs   # CLI 命令路由、exec/repl/session/config/provider 子命令
│   ├── tui.rs    # ratatui 事件循环与渲染、覆盖层、流式输出
│   ├── slash.rs  # slash 命令注册/匹配
│   └── bin/      # 预留二进制入口（当前为空）
└── AGENTS.md
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| CLI 参数和子命令 | `src/main.rs` | `Command/SessionCmd/ConfigCmd/ProviderCmd` |
| `exec` 一次性执行链路 | `src/main.rs` | `run_exec` 中创建 session、agent 并执行回合 |
| 交互模式与恢复会话 | `src/main.rs` | `run_repl` + `--resume` |
| 终端渲染与输入事件 | `src/tui.rs` | `draw`、overlay、stream buffer、status bar |
| slash 命令候选 | `src/slash.rs` | 查询与分页行为 |

## CONVENTIONS
- CLI 不直接实现业务规则，编排后委托 `zerobot-core` 执行。
- 配置入口统一走 `ConfigLoader`，避免旁路读取 yaml。
- TUI 的用户输入与审批通过 `InteractionHandler` 桥接，不在 UI 层写业务策略。

## ANTI-PATTERNS
- 在 `main.rs` 复制 core 的 provider/tool/session 初始化逻辑分叉实现。
- 在 `tui.rs` 添加与渲染无关的业务分支（应放 core）。
