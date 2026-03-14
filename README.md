# zerobot

一个基于 Rust 的多功能智能体项目骨架，采用 server/client 分离架构，HTTP + SSE 通信，支持多客户端（web/cli/tui）。

## 组件

- `zerobot-core`: 类型定义、配置、会话、工具等公共模块
- `zerobot-server`: HTTP API + SSE + Supervisor 编排
- `zerobot-sdk`: Rust SDK（reqwest）
- `zerobot-cli`: TUI + ACP(JSON-RPC stdio) 适配

## 运行

```bash
cargo run -p zerobot-server
```

```bash
cargo run -p zerobot-cli -- tui
```

## 配置

配置目录为 `.zero`，使用 YAML。加载顺序（后者覆盖前者）：

1. `~/.zero/settings.yaml`
2. `./.zero/settings.yaml`
3. `./.zero/settings.local.yaml`
4. `./.zero/managed-settings.yaml` 或环境变量 `ZEROBOT_MANAGED_SETTINGS` 指定路径

`settings.local.yaml` 已加入 `.gitignore`，用于本地覆盖。

### 示例配置

见 `.zero/settings.yaml.sample`，其中包含 LLM 供应商配置（OpenAI/Anthropic 兼容）。
