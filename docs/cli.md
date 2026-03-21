# CLI 使用说明

## 交互模式

```
zerobot
```

- 输入 `/exit` 或 `exit` 退出。
- 当输入以 `/` 开头且未进入参数段，会在信息面板显示命令建议列表。
- 使用 `↑/↓` 选择命令，`Tab` 补全。
- 未知命令会提示错误并保留输入。
- 会话自动写入 SQLite。

## 斜杠命令

```
/help [command]
/clear
/exit
/copy
/tools
/init [extra requirements]
/model [list|name]
/provider [list|id]
/config show
/session list|new [title]|show <id>
/compact
```

## 一次性执行

```
zerobot exec "请总结这段代码"
```

## 会话管理

```
zerobot session new "标题"
zerobot session list
zerobot session show <session_id>
```

## 配置查看

```
zerobot config show
zerobot config layers
```

## 提供商查看

```
zerobot provider list
```

## Gateway 常驻模式

```
zerobot gateway
```

- 启动后台 runtime，统一承载 channels / cron / heartbeat。
- 该模式下工具审批固定为 `auto`。

## Cron 管理

```
zerobot cron list --all
zerobot cron add daily-report --message "生成日报" --cron-expr "0 0 9 * * * *" --tz "Asia/Shanghai" --deliver --channel feishu --to oc_xxx
zerobot cron add remind --message "站会提醒" --every-seconds 1800
zerobot cron run <job_id> --force
zerobot cron enable <job_id>
zerobot cron disable <job_id>
zerobot cron remove <job_id>
zerobot cron status
zerobot cron export
```

## Heartbeat 管理

```
zerobot heartbeat status
zerobot heartbeat trigger
```

## CLI 覆盖

```
zerobot --set default_provider=anthropic --set default_model=claude-3-5-sonnet-latest
```
