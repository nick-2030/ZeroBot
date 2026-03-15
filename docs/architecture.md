# ZeroBot 架构设计

## 总体分层

```
CLI / TUI 层
  - 用户交互、命令解析、终端渲染
Session 层
  - 会话管理、消息循环、状态持久化
Agent 层
  - 任务编排、工具调用、权限边界
Tool 层
  - 工具注册、执行、结果处理
  - 工具输出截断
Provider 层
  - LLM 抽象与多提供商适配
Context 层
  - 上下文裁剪、系统提示词与环境信息注入
MCP 层（预留）
  - 外部工具协议扩展（MCP 本地/远程）
Infrastructure 层
  - 配置、日志、文件系统与持久化
```

## 模块结构

- `zerobot-core`：核心逻辑，提供 Agent、Session、Provider、Tool、配置加载与事件流。
- `zerobot-cli`：命令行交互，支持交互式与一次性执行。
- `zerobot-sdk`：嵌入式 SDK，直接调用 core，不依赖外部进程。

## 数据流

1. CLI/SDK 接收用户输入。
2. Agent 组装上下文与工具定义，调用 Provider。
3. Provider 返回文本与工具调用。
4. ToolRegistry 执行工具并回写结果。
5. SessionStore 记录消息、工具调用与输出。
6. 事件流（AgentEvent）向 CLI/SDK 输出执行进度。

## 扩展点

- Provider：新增供应商适配器。
- Tool：新增工具实现并注册。
- MCP：本地/远程 MCP 服务接入，工具统一注入。
- Skill：Skill 发现与按需加载。
- MCP：通过 ToolRegistry 注入 MCP 工具。
- Skill：提供 skill 工具按需加载 Skill 内容。
