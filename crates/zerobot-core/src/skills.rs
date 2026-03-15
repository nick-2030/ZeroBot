use async_trait::async_trait;

/// Skill 扩展点：目前只预留接口
#[async_trait]
pub trait Skill: Send + Sync {
    fn name(&self) -> &str;
    async fn invoke(&self, input: &str) -> String;
}
