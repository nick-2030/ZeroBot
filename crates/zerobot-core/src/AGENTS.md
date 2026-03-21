# zerobot-core/src

## OVERVIEW
运行时核心代码目录：状态机、存储、工具、provider、上下文与指令加载都在这里闭环。

## WHERE TO LOOK
| Scenario | File | Why |
|----------|------|-----|
| 单轮执行异常（卡步、死循环、无输出） | `agent.rs` | `run_turn` 主循环与 step 上限、event 发射都在此 |
| 工具参数/权限/输出异常 | `tool.rs` | 工具参数反序列化、审批、截断、read-before-write 都在此 |
| 会话丢失/历史不一致 | `session.rs` | SQLite 存储层与消息/tool/todo 读写逻辑 |
| 配置不生效 | `config.rs` | 配置层级合并、`mode_for` 与 bash 审批规则 |
| 模型响应解析失败 | `provider.rs` | OpenAI/Anthropic 流式事件与 tool call 组装 |
| Hook 行为异常 | `hooks.rs` | matcher、生效优先级、modify 浅合并 |
| 指令文件未被注入 | `instruction.rs` | 就近指令查找、URL 指令缓存、session 去重 |
| 上下文压缩/裁剪异常 | `context.rs` | max_messages/max_chars + summary anchor 逻辑 |

## HOTSPOTS
- `tool.rs`（2600+ 行）：多工具聚合 + 文件系统写路径，改动风险最高。
- `agent.rs`（900+ 行）：回合编排枢纽，任何中断条件都会影响全链路。
- `session.rs`（800+ 行）：持久化基座，schema 与查询必须同步维护。
- `provider.rs`（1000+ 行）：双提供商协议适配，流式解析脆弱点集中。

## CONVENTIONS
- 增加新工具时：先实现 `Tool`，再在 `ToolRegistry::with_builtin/with_builtin_async` 注册。
- 需要写文件的工具必须遵守：`record_file_read` → `ensure_read_before_write`。
- 新增 Hook 事件时同时更新：`HookEvent`、matcher 逻辑、触发点调用。
- 任何跨模块公共类型优先在 `lib.rs` re-export，避免上层直接依赖内部路径细节。

## ANTI-PATTERNS
- 直接在上层（CLI/SDK）拼接 provider 请求而跳过 `Agent`。
- 在 `tool.rs` 增功能但不加对应测试（尤其文件写入、patch、approval）。
- 修改 `config.rs` 字段而不更新 `example.settings.yaml` 与 `docs/config.md`。

## TESTING NOTES
- `tool.rs`、`provider.rs`、`session.rs` 以 `#[tokio::test]` 为主。
- 配置与路径逻辑优先用 `TempDir` 隔离测试环境。
