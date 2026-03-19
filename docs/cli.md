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

## CLI 覆盖

```
zerobot --set default_provider=anthropic --set default_model=claude-3-5-sonnet-latest
```
