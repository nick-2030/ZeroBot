use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, RwLock};

use crate::task::{TaskId, TaskUsage};

#[derive(Debug, Clone)]
pub struct Notification {
    pub task_id: TaskId,
    pub agent_type: String,
    pub description: String,
    pub status: NotificationStatus,
    pub result: Option<String>,
    pub usage: Option<TaskUsage>,
    pub timestamp: Instant,
}

#[derive(Debug, Clone)]
pub enum NotificationStatus {
    Completed,
    Failed(String),
    Killed,
    Progress { summary: String },
}

pub type NotificationSender = mpsc::UnboundedSender<Notification>;
pub type NotificationReceiver = mpsc::UnboundedReceiver<Notification>;

/// 进程内通知总线，按 parent_task_id 分发通知
pub struct NotificationBus {
    channels: Arc<RwLock<HashMap<TaskId, NotificationSender>>>,
}

impl NotificationBus {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 注册一个 parent task 来接收通知
    pub async fn subscribe(&self, parent_task_id: TaskId) -> NotificationReceiver {
        let (tx, rx) = mpsc::unbounded_channel();
        self.channels.write().await.insert(parent_task_id, tx);
        rx
    }

    /// 发送通知给指定 parent
    pub async fn notify(&self, parent_task_id: &TaskId, notification: Notification) {
        if let Some(tx) = self.channels.read().await.get(parent_task_id) {
            let _ = tx.send(notification);
        }
    }

    /// 取消订阅
    pub async fn unsubscribe(&self, parent_task_id: &TaskId) {
        self.channels.write().await.remove(parent_task_id);
    }
}

impl Default for NotificationBus {
    fn default() -> Self {
        Self::new()
    }
}

/// 将通知格式化为结构化消息，注入到 agent 的消息流中
pub fn format_notification_as_message(notification: &Notification) -> String {
    let status_str = match &notification.status {
        NotificationStatus::Completed => "completed",
        NotificationStatus::Failed(reason) => {
            return format!(
                "<task-notification>\n<task-id>{}</task-id>\n<agent-type>{}</agent-type>\n<status>failed</status>\n<error>{}</error>\n</task-notification>",
                notification.task_id, notification.agent_type, reason
            );
        }
        NotificationStatus::Killed => "killed",
        NotificationStatus::Progress { summary } => {
            return format!(
                "<task-notification>\n<task-id>{}</task-id>\n<agent-type>{}</agent-type>\n<status>progress</status>\n<summary>{}</summary>\n</task-notification>",
                notification.task_id, notification.agent_type, summary
            );
        }
    };

    let mut msg = format!(
        "<task-notification>\n<task-id>{}</task-id>\n<agent-type>{}</agent-type>\n<status>{}</status>",
        notification.task_id, notification.agent_type, status_str
    );

    if let Some(desc) = Some(&notification.description) {
        msg.push_str(&format!("\n<description>{}</description>", desc));
    }

    if let Some(result) = &notification.result {
        msg.push_str(&format!("\n<result>{}</result>", result));
    }

    if let Some(usage) = &notification.usage {
        msg.push_str(&format!(
            "\n<usage>\n  <total_tokens>{}</total_tokens>\n  <tool_uses>{}</tool_uses>\n  <duration_ms>{}</duration_ms>\n</usage>",
            usage.total_tokens, usage.tool_uses, usage.duration_ms
        ));
    }

    msg.push_str("\n</task-notification>");
    msg
}
