# SDK 使用说明

## 初始化

```rust
use zerobot_sdk::ZeroBot;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bot = ZeroBot::from_default_config(std::env::current_dir()?).await?;
    let session = bot.start_session(None).await?;
    let output = session.run("你好").await?;
    println!("{}", output);
    Ok(())
}
```

## 事件流

```rust
let session = bot.start_session(None).await?;
let mut rx = session.run_stream("帮我搜索项目结构").await?;
while let Some(event) = rx.recv().await {
    println!("{:?}", event);
}
```
