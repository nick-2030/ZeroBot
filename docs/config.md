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

## 字段说明

- `default_provider`：默认提供商名称。
- `default_model`：默认模型名称。
- `providers`：多提供商配置表。
- `session.db_path`：SQLite 数据库路径。
- `session.max_history`：历史消息最大数量。
- `tools.enabled`：启用的工具列表。
- `tools.allow_paths`：允许访问的路径（为空表示不限制）。
- `tools.output.max_lines`：工具输出最大行数，超出会被截断并保存完整输出到文件。
- `tools.output.max_bytes`：工具输出最大字节数。
- `tools.output.direction`：截断方向，`head` 表示保留前部，`tail` 表示保留尾部。
- `tools.output.max_lines` / `tools.output.max_bytes` 会同时生效，任一超过即截断。
- `agent.system_prompt`：系统提示词。
- `agent.max_steps`：单次回合最大步骤数。
- `logging.level`：日志级别。
- `mcp.enabled`：预留开关。
- `skills.enabled`：预留开关。
