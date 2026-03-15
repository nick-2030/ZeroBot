# 开发说明

## 构建

```
cargo build
```

## 运行

```
# 交互模式
cargo run -p zerobot-cli

# 一次性执行
cargo run -p zerobot-cli -- exec "你好"
```

## 测试

```
cargo test -p zerobot-core
```

## 目录说明

- `crates/zerobot-core` 核心库
- `crates/zerobot-cli` 命令行
- `crates/zerobot-sdk` SDK
- `docs/` 文档
- `config/` 配置示例
