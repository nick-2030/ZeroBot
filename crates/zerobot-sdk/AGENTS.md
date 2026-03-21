# zerobot-sdk

## OVERVIEW
嵌入式 SDK：向外部应用暴露 `ZeroBot` / `SessionHandle`，复用 core 的会话、工具和 agent 回合执行。

## STRUCTURE
```text
.
├── src/lib.rs   # SDK 入口与运行接口
└── AGENTS.md
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 默认配置启动 | `src/lib.rs::ZeroBot::from_default_config` | 加载配置、初始化 store/tools/hooks |
| 新建/恢复会话 | `src/lib.rs::start_session/resume_session` | 生成 `SessionHandle` |
| 同步式调用 | `src/lib.rs::SessionHandle::run` | 内部创建 `Agent` 执行回合 |
| 流式调用 | `src/lib.rs::SessionHandle::run_stream` | `mpsc::UnboundedReceiver<AgentEvent>` |

## CONVENTIONS
- SDK 作为装配层，尽量不引入独有业务语义。
- provider/model 解析逻辑与 CLI 保持一致，避免行为漂移。

## ANTI-PATTERNS
- 在 SDK 单独扩展与 core 不一致的工具注册流程。
- 直接暴露过多 core 内部细节（优先保持 `ZeroBot`/`SessionHandle` 作为稳定接口）。
