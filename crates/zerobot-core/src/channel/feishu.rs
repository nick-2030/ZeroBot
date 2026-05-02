use crate::bus::{InboundMessage, OutboundMessage};
use crate::channel::ChatChannel;
use crate::config::{FeishuChannelSettings, FeishuGroupPolicy, FeishuReactionMode};
use crate::error::{ZeroBotError, ZeroBotResult};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

#[derive(Clone)]
struct TokenCache {
    token: String,
    expires_at: i64,
}

#[derive(Default)]
struct DedupState {
    ids: VecDeque<String>,
    set: HashSet<String>,
}

impl DedupState {
    fn insert(&mut self, id: String, max: usize) -> bool {
        if self.set.contains(&id) {
            return false;
        }
        self.set.insert(id.clone());
        self.ids.push_back(id);
        while self.ids.len() > max {
            if let Some(old) = self.ids.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

#[derive(Default)]
struct WsDataCache {
    parts: HashMap<String, WsPendingData>,
}

struct WsPendingData {
    chunks: Vec<Option<Vec<u8>>>,
    trace_id: String,
    created_at_ms: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
struct PbHeader {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(string, tag = "2")]
    value: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct PbFrame {
    #[prost(uint64, tag = "1")]
    seq_id: u64,
    #[prost(uint64, tag = "2")]
    log_id: u64,
    #[prost(int32, tag = "3")]
    service: i32,
    #[prost(int32, tag = "4")]
    method: i32,
    #[prost(message, repeated, tag = "5")]
    headers: Vec<PbHeader>,
    #[prost(string, tag = "6")]
    payload_encoding: String,
    #[prost(string, tag = "7")]
    payload_type: String,
    #[prost(bytes = "vec", tag = "8")]
    payload: Vec<u8>,
    #[prost(string, tag = "9")]
    log_id_new: String,
}

const WS_FRAME_CONTROL: i32 = 0;
const WS_FRAME_DATA: i32 = 1;

#[derive(Clone)]
pub struct FeishuChannel {
    config: FeishuChannelSettings,
    inbound_tx: mpsc::UnboundedSender<InboundMessage>,
    client: reqwest::Client,
    token_cache: Arc<RwLock<Option<TokenCache>>>,
    running: Arc<AtomicBool>,
    ws_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    dedup: Arc<Mutex<DedupState>>,
    ws_data_cache: Arc<Mutex<WsDataCache>>,
}

impl FeishuChannel {
    pub fn new(
        config: FeishuChannelSettings,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
    ) -> Self {
        Self {
            config,
            inbound_tx,
            client: reqwest::Client::new(),
            token_cache: Arc::new(RwLock::new(None)),
            running: Arc::new(AtomicBool::new(false)),
            ws_task: Arc::new(Mutex::new(None)),
            dedup: Arc::new(Mutex::new(DedupState::default())),
            ws_data_cache: Arc::new(Mutex::new(WsDataCache::default())),
        }
    }

    fn base_origin(&self) -> String {
        let mut base = self
            .config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://open.feishu.cn".to_string())
            .trim_end_matches('/')
            .to_string();
        if let Some(stripped) = base.strip_suffix("/open-apis") {
            base = stripped.trim_end_matches('/').to_string();
        }
        base
    }

    fn open_api_url(&self, path: &str) -> String {
        format!(
            "{}/open-apis/{}",
            self.base_origin(),
            path.trim_start_matches('/')
        )
    }

    async fn fetch_ws_url(&self) -> ZeroBotResult<String> {
        let url = format!("{}/callback/ws/endpoint", self.base_origin());
        let resp = self
            .client
            .post(url)
            .header("locale", "zh")
            .json(&json!({
                "AppID": self.config.app_id,
                "AppSecret": self.config.app_secret,
            }))
            .send()
            .await
            .map_err(|err| ZeroBotError::Http(err.to_string()))?;

        let status = resp.status();
        let raw = resp
            .json::<JsonValue>()
            .await
            .map_err(|err| ZeroBotError::Http(err.to_string()))?;
        let body = serde_json::from_value::<WsEndpointResp>(raw.clone()).map_err(|err| {
            ZeroBotError::Http(format!("feishu ws endpoint parse failed: {err}, raw={raw}"))
        })?;
        if !status.is_success() {
            return Err(ZeroBotError::Http(format!(
                "feishu ws endpoint http {}: {}",
                status, raw
            )));
        }
        if body.code != 0 {
            return Err(ZeroBotError::Http(format!(
                "feishu ws endpoint error code={} msg={}",
                body.code, body.msg
            )));
        }
        let ws_url = body.data.and_then(|d| d.url).ok_or_else(|| {
            ZeroBotError::Http(format!("feishu ws endpoint url missing, raw={raw}"))
        })?;
        Ok(ws_url)
    }

    async fn tenant_access_token(&self) -> ZeroBotResult<String> {
        let now = chrono::Utc::now().timestamp();
        {
            let cache = self.token_cache.read().await;
            if let Some(cache) = cache.as_ref() {
                if cache.expires_at > now + 30 {
                    debug!("feishu token cache hit");
                    return Ok(cache.token.clone());
                }
            }
        }

        let url = self.open_api_url("auth/v3/tenant_access_token/internal");
        let resp = self
            .client
            .post(url)
            .json(&json!({
                "app_id": self.config.app_id,
                "app_secret": self.config.app_secret,
            }))
            .send()
            .await
            .map_err(|err| ZeroBotError::Http(err.to_string()))?;
        let status = resp.status();
        let body = resp
            .json::<JsonValue>()
            .await
            .map_err(|err| ZeroBotError::Http(err.to_string()))?;
        if !status.is_success() || body.get("code").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
            return Err(ZeroBotError::Http(format!(
                "获取飞书 tenant_access_token 失败: {}",
                body
            )));
        }
        let token = body
            .get("tenant_access_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if token.is_empty() {
            return Err(ZeroBotError::Http(
                "飞书 tenant_access_token 为空".to_string(),
            ));
        }
        let expire = body.get("expire").and_then(|v| v.as_i64()).unwrap_or(7200);
        *self.token_cache.write().await = Some(TokenCache {
            token: token.clone(),
            expires_at: chrono::Utc::now().timestamp() + expire,
        });
        info!("feishu token refreshed, expires_in={}s", expire);
        Ok(token)
    }

    async fn ws_loop(self) {
        info!(
            "feishu ws loop started: base_url={}, app_id_len={}",
            self.base_origin(),
            self.config.app_id.len()
        );

        while self.running.load(Ordering::SeqCst) {
            let ws_url = match self.fetch_ws_url().await {
                Ok(url) => url,
                Err(err) => {
                    warn!("feishu ws endpoint fetch failed: {}", err);
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            debug!("feishu ws endpoint acquired");
            debug!("feishu ws connecting");
            match connect_async(&ws_url).await {
                Ok((mut ws, _)) => {
                    info!("feishu ws connected");
                    while self.running.load(Ordering::SeqCst) {
                        let msg = ws.next().await;
                        let Some(msg) = msg else {
                            warn!("feishu ws stream ended");
                            break;
                        };
                        match msg {
                            Ok(Message::Text(text)) => {
                                if let Some(reply) = self.handle_ws_text(&text).await {
                                    if let Err(err) = ws.send(Message::Text(reply)).await {
                                        warn!("feishu ws pong send failed: {}", err);
                                    }
                                }
                            }
                            Ok(Message::Binary(payload)) => {
                                if let Some(reply) = self.handle_ws_binary(&payload).await {
                                    if let Err(err) = ws.send(Message::Binary(reply)).await {
                                        warn!("feishu ws binary reply send failed: {}", err);
                                    }
                                }
                            }
                            Ok(Message::Ping(payload)) => {
                                if let Err(err) = ws.send(Message::Pong(payload)).await {
                                    warn!("feishu ws ping-pong failed: {}", err);
                                }
                            }
                            Ok(Message::Close(frame)) => {
                                info!("feishu ws closed by remote: {:?}", frame);
                                break;
                            }
                            Ok(_) => {}
                            Err(err) => {
                                warn!("feishu ws receive failed: {}", err);
                                break;
                            }
                        }
                    }
                }
                Err(err) => {
                    warn!("feishu ws connect failed: {}", err);
                    if err.to_string().contains("404") {
                        warn!(
                            "feishu ws endpoint returned 404, verify long-connection is enabled and app credentials are correct"
                        );
                    }
                }
            }
            sleep(Duration::from_secs(5)).await;
        }
        info!("feishu ws loop stopped");
    }

    async fn handle_ws_text(&self, text: &str) -> Option<String> {
        let raw: JsonValue = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(err) => {
                warn!(
                    "feishu ws text parse failed: {}, body_len={}",
                    err,
                    text.len()
                );
                return None;
            }
        };
        if raw.get("type").and_then(|v| v.as_str()) == Some("ping") {
            debug!("feishu ws ping received");
            return Some(json!({"type":"pong"}).to_string());
        }
        if let Err(err) = self.handle_ws_event_json(&raw).await {
            warn!("feishu text event handle failed: {}", err);
        }
        None
    }

    async fn handle_ws_binary(&self, payload: &[u8]) -> Option<Vec<u8>> {
        let frame = match PbFrame::decode(payload) {
            Ok(v) => v,
            Err(err) => {
                warn!("feishu ws binary decode failed: {}", err);
                return None;
            }
        };
        debug!(
            "feishu ws frame received: method={}, service={}, payload_bytes={}",
            frame.method,
            frame.service,
            frame.payload.len()
        );

        if frame.method == WS_FRAME_CONTROL {
            self.handle_ws_control_frame(&frame).await;
            return None;
        }
        if frame.method != WS_FRAME_DATA {
            debug!("feishu ws frame skipped by method={}", frame.method);
            return None;
        }

        let msg_type = header_value(&frame.headers, "type").unwrap_or_default();
        if msg_type != "event" && msg_type != "card" {
            debug!("feishu ws frame skipped by header type={}", msg_type);
            return None;
        }

        let message_id = header_value(&frame.headers, "message_id").unwrap_or_default();
        let sum = header_value(&frame.headers, "sum")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        let seq = header_value(&frame.headers, "seq")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let trace_id = header_value(&frame.headers, "trace_id").unwrap_or_default();

        let merged_payload = self
            .merge_ws_payload(&message_id, sum, seq, &trace_id, frame.payload.clone())
            .await?;

        let raw: JsonValue = match serde_json::from_slice(&merged_payload) {
            Ok(v) => v,
            Err(err) => {
                warn!(
                    "feishu ws merged payload parse failed: {}, bytes={}",
                    err,
                    merged_payload.len()
                );
                return Some(build_ws_ack_frame(frame, 500));
            }
        };
        if let Err(err) = self.handle_ws_event_json(&raw).await {
            warn!("feishu binary event handle failed: {}", err);
            return Some(build_ws_ack_frame(frame, 500));
        }
        Some(build_ws_ack_frame(frame, 200))
    }

    async fn handle_ws_control_frame(&self, frame: &PbFrame) {
        if let Some(status) = header_value(&frame.headers, "handshake-status") {
            if status != "0" {
                let msg = header_value(&frame.headers, "handshake-msg").unwrap_or_default();
                let auth =
                    header_value(&frame.headers, "handshake-autherrcode").unwrap_or_default();
                warn!(
                    "feishu ws handshake failed: status={}, msg={}, autherr={}",
                    status, msg, auth
                );
            }
        }
        if let Some(msg_type) = header_value(&frame.headers, "type") {
            debug!("feishu ws control frame type={}", msg_type);
        }
    }

    async fn merge_ws_payload(
        &self,
        message_id: &str,
        sum: usize,
        seq: usize,
        trace_id: &str,
        data: Vec<u8>,
    ) -> Option<Vec<u8>> {
        if message_id.is_empty() {
            return Some(data);
        }
        if seq >= sum {
            warn!(
                "feishu ws chunk index out of range: message_id={}, seq={}, sum={}",
                message_id, seq, sum
            );
            return None;
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut cache = self.ws_data_cache.lock().await;
        cache
            .parts
            .retain(|_, state| now_ms.saturating_sub(state.created_at_ms) <= 10_000);

        let entry = cache
            .parts
            .entry(message_id.to_string())
            .or_insert_with(|| WsPendingData {
                chunks: vec![None; sum],
                trace_id: trace_id.to_string(),
                created_at_ms: now_ms,
            });
        if entry.chunks.len() != sum {
            entry.chunks = vec![None; sum];
            entry.created_at_ms = now_ms;
            entry.trace_id = trace_id.to_string();
        }
        entry.chunks[seq] = Some(data);

        if entry.chunks.iter().any(|v| v.is_none()) {
            debug!(
                "feishu ws chunk cached: message_id={}, seq={}/{}, trace_id={}",
                message_id, seq, sum, trace_id
            );
            return None;
        }

        let mut merged = Vec::new();
        for bytes in entry.chunks.iter().flatten() {
            merged.extend_from_slice(bytes);
        }
        cache.parts.remove(message_id);
        Some(merged)
    }

    async fn handle_ws_event_json(&self, raw: &JsonValue) -> ZeroBotResult<()> {
        let event_type = raw
            .get("header")
            .and_then(|h| h.get("event_type"))
            .and_then(|v| v.as_str());
        debug!("feishu ws event_type={:?}", event_type);
        match event_type {
            Some("im.message.receive_v1") => {
                if let Some(event) = raw.get("event") {
                    self.handle_message_event(event).await?;
                }
            }
            Some("im.message.reaction.created_v1") => {
                if let Some(event) = raw.get("event") {
                    self.handle_reaction_event(event).await?;
                }
            }
            Some(other) => {
                debug!("feishu ws event ignored: event_type={}", other);
            }
            None => {
                debug!("feishu ws payload missing header.event_type");
            }
        }
        Ok(())
    }

    async fn handle_message_event(&self, event: &JsonValue) -> ZeroBotResult<()> {
        let message = event
            .get("message")
            .ok_or_else(|| ZeroBotError::Http("飞书事件缺少 message".to_string()))?;
        let sender = event
            .get("sender")
            .and_then(|v| v.get("sender_id"))
            .and_then(|v| v.get("open_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        debug!("feishu inbound message: sender={}", sender);
        if !self.is_allowed(sender) {
            info!("feishu inbound skipped by allow_from: sender={}", sender);
            return Ok(());
        }

        let message_id = message
            .get("message_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if !message_id.is_empty() {
            let mut dedup = self.dedup.lock().await;
            if !dedup.insert(message_id.clone(), self.config.dedup_max_entries.max(100)) {
                debug!("feishu inbound skipped by dedup: message_id={}", message_id);
                return Ok(());
            }
        }

        let chat_id = message
            .get("chat_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let chat_type = message
            .get("chat_type")
            .and_then(|v| v.as_str())
            .unwrap_or("p2p");
        let msg_type = message
            .get("message_type")
            .and_then(|v| v.as_str())
            .unwrap_or("text");
        if chat_type == "group"
            && matches!(self.config.group_policy, FeishuGroupPolicy::Mention)
            && !is_bot_mentioned(message)
        {
            info!(
                "feishu inbound skipped by group mention policy: chat_id={}, message_id={}",
                chat_id, message_id
            );
            return Ok(());
        }

        let mut content = parse_message_content(msg_type, message.get("content"));
        if let Some(parent_id) = message.get("parent_id").and_then(|v| v.as_str()) {
            if !parent_id.is_empty() {
                if let Ok(Some(parent)) = self.fetch_message_text(parent_id).await {
                    content = format!("[Reply to: {parent}]\n{content}");
                }
            }
        }

        let thread = message
            .get("thread_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let session_key = if chat_type == "group" && !thread.is_empty() {
            format!("feishu:{chat_id}:{thread}")
        } else {
            format!("feishu:{chat_id}")
        };

        let metadata = json!({
            "message_id": message_id.clone(),
            "chat_type": chat_type,
            "msg_type": msg_type,
            "parent_id": message.get("parent_id").and_then(|v| v.as_str()),
            "root_id": message.get("root_id").and_then(|v| v.as_str()),
            "thread_id": thread,
        });

        let inbound = InboundMessage::new(
            "feishu",
            session_key,
            "feishu",
            sender,
            chat_id,
            content,
            metadata,
        );
        if let Err(err) = self.inbound_tx.send(inbound) {
            warn!("feishu inbound enqueue failed: {}", err);
            return Err(ZeroBotError::Io(err.to_string()));
        }
        info!(
            "feishu inbound accepted: message_id={}, chat_type={}, msg_type={}",
            message_id, chat_type, msg_type
        );
        Ok(())
    }

    async fn handle_reaction_event(&self, event: &JsonValue) -> ZeroBotResult<()> {
        if matches!(self.config.reaction_mode, FeishuReactionMode::Off) {
            debug!("feishu reaction skipped: mode=off");
            return Ok(());
        }
        let message_id = event
            .get("message_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if message_id.is_empty() {
            debug!("feishu reaction skipped: empty message_id");
            return Ok(());
        }

        let emoji = event
            .get("reaction_type")
            .and_then(|v| v.get("emoji_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("UNKNOWN")
            .to_string();
        let operator = event
            .get("user_id")
            .and_then(|v| v.get("open_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        if operator == self.config.bot_open_id.clone().unwrap_or_default() {
            debug!(
                "feishu reaction skipped: self reaction operator={}",
                operator
            );
            return Ok(());
        }
        if !self.is_allowed(&operator) {
            info!(
                "feishu reaction skipped by allow_from: operator={}, message_id={}",
                operator, message_id
            );
            return Ok(());
        }

        let message_info = self.fetch_message_info(&message_id).await.ok().flatten();
        if matches!(self.config.reaction_mode, FeishuReactionMode::Own) {
            let own = message_info
                .as_ref()
                .and_then(|m| m.get("sender_type"))
                .and_then(|v| v.as_str())
                == Some("app");
            if !own {
                debug!(
                    "feishu reaction skipped by own mode: operator={}, message_id={}",
                    operator, message_id
                );
                return Ok(());
            }
        }

        let chat_id = message_info
            .as_ref()
            .and_then(|m| m.get("chat_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("reaction")
            .to_string();
        let thread = message_info
            .as_ref()
            .and_then(|m| m.get("thread_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let session_key = if !thread.is_empty() {
            format!("feishu:{chat_id}:{thread}")
        } else {
            format!("feishu:{chat_id}")
        };
        let content = format!("[reacted with {emoji} to message {message_id}]");
        let metadata = json!({
            "reaction": true,
            "message_id": message_id.clone(),
            "emoji": emoji.clone(),
        });
        self.inbound_tx
            .send(InboundMessage::new(
                "feishu",
                session_key,
                "feishu",
                operator,
                chat_id,
                content,
                metadata,
            ))
            .map_err(|err| ZeroBotError::Io(err.to_string()))?;
        info!("feishu reaction accepted: message_id={}", message_id);
        Ok(())
    }

    fn is_allowed(&self, sender: &str) -> bool {
        if self.config.allow_from.is_empty() {
            return false;
        }
        if self.config.allow_from.iter().any(|v| v == "*") {
            return true;
        }
        self.config.allow_from.iter().any(|v| v == sender)
    }

    async fn fetch_message_text(&self, message_id: &str) -> ZeroBotResult<Option<String>> {
        let info = self.fetch_message_info(message_id).await?;
        Ok(info.and_then(|obj| obj.get("content").and_then(extract_message_text)))
    }

    async fn fetch_message_info(&self, message_id: &str) -> ZeroBotResult<Option<JsonValue>> {
        let token = self.tenant_access_token().await?;
        let url = self.open_api_url(&format!("im/v1/messages/{message_id}"));
        let resp = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|err| ZeroBotError::Http(err.to_string()))?;
        if !resp.status().is_success() {
            debug!(
                "feishu fetch_message_info non-success: message_id={}, status={}",
                message_id,
                resp.status()
            );
            return Ok(None);
        }
        let raw = resp
            .json::<JsonValue>()
            .await
            .map_err(|err| ZeroBotError::Http(err.to_string()))?;
        let data = raw.get("data").cloned().unwrap_or(raw.clone());
        Ok(Some(data))
    }

    async fn send_message(
        &self,
        chat_id: &str,
        msg_type: &str,
        content: String,
        reply_to: Option<&str>,
    ) -> ZeroBotResult<()> {
        let token = self.tenant_access_token().await?;
        if let Some(message_id) = reply_to {
            debug!(
                "feishu outbound reply: chat_id={}, message_id={}, msg_type={}",
                chat_id, message_id, msg_type
            );
            let url = self.open_api_url(&format!("im/v1/messages/{message_id}/reply"));
            let resp = self
                .client
                .post(url)
                .bearer_auth(token)
                .json(&json!({
                    "msg_type": msg_type,
                    "content": content,
                }))
                .send()
                .await
                .map_err(|err| ZeroBotError::Http(err.to_string()))?;
            let _ = parse_feishu_json_response(resp, "send_reply").await?;
            return Ok(());
        }

        debug!(
            "feishu outbound send: chat_id={}, msg_type={}",
            chat_id, msg_type
        );
        let url = self.open_api_url("im/v1/messages?receive_id_type=chat_id");
        let resp = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&json!({
                "receive_id": chat_id,
                "msg_type": msg_type,
                "content": content,
            }))
            .send()
            .await
            .map_err(|err| ZeroBotError::Http(err.to_string()))?;
        let _ = parse_feishu_json_response(resp, "send_message").await?;
        Ok(())
    }

    async fn upload_file(&self, path: &str) -> ZeroBotResult<Option<String>> {
        let ext = Path::new(path)
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let token = self.tenant_access_token().await?;
        let file = tokio::fs::read(path)
            .await
            .map_err(|err| ZeroBotError::Io(err.to_string()))?;
        let filename = Path::new(path)
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("upload.bin")
            .to_string();
        debug!("feishu upload start: path={}, ext={}", path, ext);

        if ["png", "jpg", "jpeg", "gif", "webp"].contains(&ext.as_str()) {
            let url = self.open_api_url("im/v1/images");
            let form = Form::new()
                .text("image_type", "message")
                .part("image", Part::bytes(file).file_name(filename));
            let resp = self
                .client
                .post(url)
                .bearer_auth(token)
                .multipart(form)
                .send()
                .await
                .map_err(|err| ZeroBotError::Http(err.to_string()))?;
            let raw = parse_feishu_json_response(resp, "upload_image").await?;
            let key = raw
                .get("data")
                .and_then(|d| d.get("image_key"))
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
            info!(
                "feishu upload image done: path={}, has_key={}",
                path,
                key.is_some()
            );
            return Ok(key);
        }

        let url = self.open_api_url("im/v1/files");
        let form = Form::new()
            .text("file_type", "stream")
            .text("file_name", filename)
            .part("file", Part::bytes(file).file_name("upload.bin"));
        let resp = self
            .client
            .post(url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .map_err(|err| ZeroBotError::Http(err.to_string()))?;
        let raw = parse_feishu_json_response(resp, "upload_file").await?;
        let key = raw
            .get("data")
            .and_then(|d| d.get("file_key"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        info!(
            "feishu upload file done: path={}, has_key={}",
            path,
            key.is_some()
        );
        Ok(key)
    }
}

#[derive(Debug, Deserialize)]
struct WsEndpointResp {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    data: Option<WsEndpointData>,
}

#[derive(Debug, Deserialize)]
struct WsEndpointData {
    #[serde(default)]
    #[serde(alias = "URL")]
    #[serde(alias = "Url")]
    url: Option<String>,
}

#[async_trait]
impl ChatChannel for FeishuChannel {
    fn name(&self) -> &str {
        "feishu"
    }

    async fn start(&self) -> ZeroBotResult<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        info!(
            "feishu channel starting: group_policy={:?}, reaction_mode={:?}, allow_from_count={}",
            self.config.group_policy,
            self.config.reaction_mode,
            self.config.allow_from.len()
        );
        let this = self.clone();
        let mut guard = self.ws_task.lock().await;
        *guard = Some(tokio::spawn(async move {
            this.ws_loop().await;
        }));
        Ok(())
    }

    async fn stop(&self) -> ZeroBotResult<()> {
        info!("feishu channel stopping");
        self.running.store(false, Ordering::SeqCst);
        let mut guard = self.ws_task.lock().await;
        if let Some(task) = guard.take() {
            task.abort();
        }
        info!("feishu channel stopped");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> ZeroBotResult<()> {
        info!(
            "feishu outbound queued: chat_id={}, content_len={}, media_count={}",
            msg.chat_id,
            msg.content.len(),
            msg.media.len()
        );
        let reply_to = if self.config.reply_to_message {
            msg.metadata.get("message_id").and_then(|v| v.as_str())
        } else {
            None
        };

        for media in &msg.media {
            if let Some(key) = self.upload_file(media).await? {
                let is_image = media.ends_with(".png")
                    || media.ends_with(".jpg")
                    || media.ends_with(".jpeg")
                    || media.ends_with(".gif")
                    || media.ends_with(".webp");
                if is_image {
                    self.send_message(
                        &msg.chat_id,
                        "image",
                        json!({"image_key": key}).to_string(),
                        reply_to,
                    )
                    .await?;
                } else {
                    self.send_message(
                        &msg.chat_id,
                        "file",
                        json!({"file_key": key}).to_string(),
                        reply_to,
                    )
                    .await?;
                }
            }
        }

        if msg.content.trim().is_empty() {
            return Ok(());
        }

        let format = detect_message_format(&msg.content);
        match format.as_str() {
            "text" => {
                self.send_message(
                    &msg.chat_id,
                    "text",
                    json!({"text": msg.content}).to_string(),
                    reply_to,
                )
                .await?;
            }
            "post" => {
                self.send_message(
                    &msg.chat_id,
                    "post",
                    json!({
                        "zh_cn": {
                            "content": [[{"tag":"md", "text": msg.content}]]
                        }
                    })
                    .to_string(),
                    reply_to,
                )
                .await?;
            }
            _ => {
                self.send_message(
                    &msg.chat_id,
                    "interactive",
                    json!({
                        "config": {"wide_screen_mode": true},
                        "elements": [{"tag":"markdown","content": msg.content}]
                    })
                    .to_string(),
                    reply_to,
                )
                .await?;
            }
        }
        Ok(())
    }
}

fn header_value(headers: &[PbHeader], key: &str) -> Option<String> {
    headers
        .iter()
        .find(|h| h.key == key)
        .map(|h| h.value.clone())
}

fn build_ws_ack_frame(frame: PbFrame, code: i32) -> Vec<u8> {
    let mut headers = frame.headers.clone();
    headers.push(PbHeader {
        key: "biz_rt".to_string(),
        value: "0".to_string(),
    });
    let payload = json!({ "code": code });
    let resp = PbFrame {
        seq_id: frame.seq_id,
        log_id: frame.log_id,
        service: frame.service,
        method: frame.method,
        headers,
        payload_encoding: frame.payload_encoding,
        payload_type: frame.payload_type,
        payload: payload.to_string().into_bytes(),
        log_id_new: frame.log_id_new,
    };
    resp.encode_to_vec()
}

async fn parse_feishu_json_response(resp: reqwest::Response, op: &str) -> ZeroBotResult<JsonValue> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| ZeroBotError::Http(err.to_string()))?;
    let raw = serde_json::from_str::<JsonValue>(&text).unwrap_or_else(|_| json!({ "raw": text }));
    let code = raw.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
    if !status.is_success() || code != 0 {
        warn!(
            "feishu api {} failed: status={}, code={}, body={}",
            op, status, code, raw
        );
        return Err(ZeroBotError::Http(format!(
            "feishu api {} failed: status={} code={}",
            op, status, code
        )));
    }
    debug!("feishu api {} success", op);
    Ok(raw)
}

fn is_bot_mentioned(message: &JsonValue) -> bool {
    let raw_content = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if raw_content.contains("@_all") {
        return true;
    }
    message
        .get("mentions")
        .and_then(|v| v.as_array())
        .map(|mentions| !mentions.is_empty())
        .unwrap_or(false)
}

fn parse_message_content(msg_type: &str, raw_content: Option<&JsonValue>) -> String {
    let content_str = raw_content.and_then(|v| v.as_str()).unwrap_or("{}");
    let parsed = serde_json::from_str::<JsonValue>(content_str).unwrap_or_else(|_| json!({}));
    match msg_type {
        "text" => parsed
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "post" => extract_message_text(&parsed).unwrap_or_else(|| "[post]".to_string()),
        "image" => "[image]".to_string(),
        "audio" => "[audio]".to_string(),
        "file" => "[file]".to_string(),
        _ => format!("[{msg_type}]"),
    }
}

fn extract_message_text(raw: &JsonValue) -> Option<String> {
    if let Some(text) = raw.get("text").and_then(|v| v.as_str()) {
        return Some(text.to_string());
    }
    let root = raw.get("post").unwrap_or(raw);
    if let Some(content) = root
        .get("zh_cn")
        .or_else(|| root.get("en_us"))
        .or_else(|| root.get("ja_jp"))
        .and_then(|v| v.get("content"))
        .and_then(|v| v.as_array())
    {
        let mut out = String::new();
        for row in content {
            if let Some(items) = row.as_array() {
                for item in items {
                    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                        if !out.is_empty() {
                            out.push(' ');
                        }
                        out.push_str(text);
                    }
                }
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

fn detect_message_format(content: &str) -> String {
    if content.contains("```")
        || content.contains('|')
        || content
            .lines()
            .any(|line| line.trim_start().starts_with('#'))
        || content
            .lines()
            .any(|line| line.trim_start().starts_with("- "))
        || content.contains("**")
    {
        return "interactive".to_string();
    }
    if content.contains("http://") || content.contains("https://") {
        return "post".to_string();
    }
    if content.chars().count() <= 200 {
        return "text".to_string();
    }
    "post".to_string()
}
