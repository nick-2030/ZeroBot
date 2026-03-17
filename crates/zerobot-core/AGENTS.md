# zerobot-core

## 概述
ZeroBot 的核心编排引擎，管理 Agent、会话、Provider、工具和模型上下文协议（MCP）。

## 结构
```
.
├── agent.rs       # Agent 生命周期和决策循环
├── session.rs     # 对话历史和状态
├── tool.rs        # 工具模式定义和执行
├── mcp.rs         # 模型上下文协议集成
├── provider.rs    # LLM Provider 抽象
├── prompt.rs      # 系统提示词生成
├── skills.rs      # Agent 技能
├── hooks.rs       # 执行钩子
├── config.rs      # 设置和配置
├── logging.rs     # 追踪和日志设置
└── events.rs      # 事件系统
```

## 查找位置
| 任务 | 位置 | 备注 |
|------|------|------|
| Agent 逻辑 | `agent.rs` | AI 编排的核心 |
| MCP 工具 | `mcp.rs` | 与 MCP 服务器交互 |
| 会话存储 | `session.rs` | 保存/加载历史 |
| LLM 后端 | `provider.rs` | 切换 AI 模型 |
