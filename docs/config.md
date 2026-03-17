# 配置说明（YAML）

## 作用域与优先级

配置读取顺序从低到高：

1. Defaults（内置默认）
2. User：`~/.zerobot/settings.yaml`
3. Project：`./.zerobot/settings.yaml`
4. Local：`./.zerobot/settings.local.yaml`
5. Managed：`/etc/zerobot/managed-settings.yaml`（企业可选）
6. CLI 覆盖：`--set key=value`

最终生效优先级：`CLI > Managed > Local > Project > User > Defaults`。

## 与 Git 忽略的关系

- 当项目根目录的 `.gitignore` 忽略了 `.zerobot/` 时，Project 配置不会被读取。
- `settings.local.yaml` 必须被 `.gitignore` 忽略，避免提交到仓库。

## 配置示例

参考 `config/example.settings.yaml`。
系统提示词的说明见 `docs/prompt.md`。

## 字段说明

- `default_provider`：默认提供商名称。
- `default_model`：默认模型名称。
- `providers`：多提供商配置表。
- `session.db_path`：SQLite 数据库路径。
- `session.max_history`：历史消息最大数量。
- `context.max_messages`：上下文保留的最大消息条数（0 表示不限制）。
- `context.max_chars`：上下文保留的最大字符数（0 表示不限制）。
- `context.max_tokens`：上下文最大 token 预算（可选）。未配置时自动压缩不生效。
- `context.model_limits`：按模型设置上下文上限（可选，优先于 `max_tokens`）。
- `context.include_environment`：是否在系统提示词中注入环境信息。
- `context.compaction.enabled`：是否启用上下文压缩（默认 true）。
- `context.compaction.auto`：是否自动触发压缩（默认 true）。
- `context.compaction.reserved_tokens`：压缩预留 token（默认 2048）。
- `context.compaction.summary_model`：摘要使用的模型（可选，默认跟随对话模型）。
- `instructions`：额外指令来源列表，支持绝对/相对路径、glob 与 URL。
- `tools.enabled`：启用的工具列表。
- `tools.allow_paths`：允许访问的路径（为空表示不限制）。
- `tools.output.max_lines`：工具输出最大行数，超出会被截断。
- `tools.output.max_bytes`：工具输出最大字节数。
- `tools.output.direction`：截断方向，`head` 表示保留前部，`tail` 表示保留尾部。
- `tools.output.max_lines` / `tools.output.max_bytes` 会同时生效，任一超过即截断。
- `todoread` / `todowrite`：读取与更新会话内 Todo 列表（当 `tools.enabled` 包含时可用）。
- `agent.system_prompt`：系统提示词。
- `agent.max_steps`：单次回合最大步骤数。
- `logging.level`：日志级别。
- `mcp.enabled`：是否启用 MCP。
- `mcp.servers`：MCP 服务器列表。
  - `name`：服务器名称。
  - `type`：`local` 或 `remote`。
  - `command`：本地 MCP 启动命令（local）。
  - `env`：本地 MCP 环境变量（local）。
  - `protocol`：本地 MCP 协议（`content_length` 或 `line`，默认 `content_length`）。
  - `url`：远程 MCP 地址（remote）。
  - `headers`：远程 MCP 额外请求头（remote）。
  - `timeout_ms`：请求超时毫秒数。
  - `enabled`：是否启用该服务器。
- `skills.enabled`：是否启用 Skill。
- `skills.paths`：额外 Skill 目录列表。

## 子代理与 Hook

### 子代理目录

- 项目级：`./.zerobot/agents/*.md`
- 用户级：`~/.zerobot/agents/*.md`

子代理文件必须包含 frontmatter，最小格式如下：

```
---
name: demo
description: "用于执行复杂检索或子任务"
model: "gpt-4o-mini" # 可选，默认继承主模型
tools: "read,grep,glob" # 可选，留空表示继承
hooks: [] # 可选
---

这里是子代理的提示词正文，会作为子代理的系统提示词。`tools` 支持逗号分隔字符串或 YAML 列表。
```

### Hook 目录

- 项目级：`./.zerobot/hooks/*.yaml`
Hook 也可以写在子代理或 Skill 的 frontmatter 里，优先级：agent > skill > hooks 目录。

Hook 文件格式：

```
hooks:
  - name: "guard"
    command: ["bash", "-lc", "echo '{\"action\":\"allow\"}'"]
    matcher: "shell"
    timeout_ms: 3000
    enabled: true
    events: ["pre_tool_use", "post_tool_use"]
```

### Hook 协议

- stdin JSON：`{hook, session_id, payload}`
- stdout JSON：`{action, message, patch}`
  - `action`：`allow | deny | modify`
  - `modify` 时会对 payload 做浅合并

### Hook 事件

`matcher` 目前仅对工具事件生效（匹配 `tool_name`），支持精确匹配或 `*` 通配。

- `session_start`：会话创建后触发。
- `session_end`：会话结束时触发。
- `user_prompt_submit`：用户输入提交时触发。
- `message_append`：追加消息时触发。
- `pre_tool_use`：工具执行前触发。
- `post_tool_use`：工具执行后触发。
- `post_tool_use_failure`：工具执行失败触发。
- `subagent_start`：子代理启动时触发。
- `subagent_stop`：子代理结束时触发。
- `task_completed`：当前回合完成时触发。
- `stop`：当前回合停止时触发。
- `pre_provider`：调用模型前触发。
- `post_provider`：模型响应后触发。

## Skill 执行规则（内置）

- 当调用 `skill` 工具时，会把 Skill 信息压入当前会话的 Skill 栈。
- 完成 Skill 后，必须调用 `skill` 工具并设置 `action: end` 来出栈。
- 只要 Skill 栈未清空，会话不会结束，会自动要求继续完成 Skill。
- `skill` 工具输出包含 `name` 与 `path`，便于使用 `read/glob/grep` 读取 Skill 目录内的文件。
