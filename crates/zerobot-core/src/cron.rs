use crate::error::{ZeroBotError, ZeroBotResult};
use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Row, Sqlite};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CronScheduleKind {
    At,
    Every,
    Cron,
}

impl CronScheduleKind {
    fn as_str(self) -> &'static str {
        match self {
            CronScheduleKind::At => "at",
            CronScheduleKind::Every => "every",
            CronScheduleKind::Cron => "cron",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "at" => Some(CronScheduleKind::At),
            "every" => Some(CronScheduleKind::Every),
            "cron" => Some(CronScheduleKind::Cron),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronSchedule {
    pub kind: CronScheduleKind,
    #[serde(default)]
    pub at_ms: Option<i64>,
    #[serde(default)]
    pub every_ms: Option<i64>,
    #[serde(default)]
    pub expr: Option<String>,
    #[serde(default)]
    pub tz: Option<String>,
}

impl CronSchedule {
    pub fn at(at_ms: i64) -> Self {
        Self {
            kind: CronScheduleKind::At,
            at_ms: Some(at_ms),
            every_ms: None,
            expr: None,
            tz: None,
        }
    }

    pub fn every(every_ms: i64) -> Self {
        Self {
            kind: CronScheduleKind::Every,
            at_ms: None,
            every_ms: Some(every_ms),
            expr: None,
            tz: None,
        }
    }

    pub fn cron(expr: impl Into<String>, tz: Option<String>) -> Self {
        Self {
            kind: CronScheduleKind::Cron,
            at_ms: None,
            every_ms: None,
            expr: Some(expr.into()),
            tz,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronPayload {
    #[serde(default = "default_payload_kind")]
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub deliver: bool,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

fn default_payload_kind() -> String {
    "agent_turn".to_string()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CronRunStatus {
    Ok,
    Error,
    Skipped,
}

impl CronRunStatus {
    fn as_str(self) -> &'static str {
        match self {
            CronRunStatus::Ok => "ok",
            CronRunStatus::Error => "error",
            CronRunStatus::Skipped => "skipped",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "ok" => Some(CronRunStatus::Ok),
            "error" => Some(CronRunStatus::Error),
            "skipped" => Some(CronRunStatus::Skipped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronRunRecord {
    pub run_at_ms: i64,
    pub status: CronRunStatus,
    pub duration_ms: i64,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronJobState {
    #[serde(default)]
    pub next_run_at_ms: Option<i64>,
    #[serde(default)]
    pub last_run_at_ms: Option<i64>,
    #[serde(default)]
    pub last_status: Option<CronRunStatus>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub run_history: Vec<CronRunRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub schedule: CronSchedule,
    pub payload: CronPayload,
    pub state: CronJobState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub delete_after_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronStoreSnapshot {
    pub version: u32,
    pub jobs: Vec<CronJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronServiceStatus {
    pub enabled: bool,
    pub jobs: usize,
    pub next_wake_at_ms: Option<i64>,
}

pub type CronJobHandler =
    Arc<dyn Fn(CronJob) -> BoxFuture<'static, ZeroBotResult<()>> + Send + Sync>;

#[derive(Clone)]
pub struct CronService {
    pool: Pool<Sqlite>,
    export_path: Option<PathBuf>,
    run_history_limit: usize,
    running: Arc<AtomicBool>,
    timer_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    handler: Arc<RwLock<Option<CronJobHandler>>>,
    execution_lock: Arc<Mutex<()>>,
}

impl CronService {
    pub async fn new(
        db_path: impl AsRef<Path>,
        export_path: Option<PathBuf>,
        run_history_limit: usize,
    ) -> ZeroBotResult<Self> {
        let db_path = db_path.as_ref();
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        }
        let url = format!("sqlite://{}", db_path.to_string_lossy());
        let opts = SqliteConnectOptions::from_str(&url)
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;

        let svc = Self {
            pool,
            export_path,
            run_history_limit: run_history_limit.max(1),
            running: Arc::new(AtomicBool::new(false)),
            timer_task: Arc::new(Mutex::new(None)),
            handler: Arc::new(RwLock::new(None)),
            execution_lock: Arc::new(Mutex::new(())),
        };
        svc.init().await?;
        Ok(svc)
    }

    pub fn set_handler(&self, handler: Option<CronJobHandler>) {
        let store = self.handler.clone();
        tokio::spawn(async move {
            *store.write().await = handler;
        });
    }

    pub async fn init(&self) -> ZeroBotResult<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS cron_jobs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                schedule_kind TEXT NOT NULL,
                schedule_at_ms INTEGER,
                schedule_every_ms INTEGER,
                schedule_expr TEXT,
                schedule_tz TEXT,
                payload_kind TEXT NOT NULL,
                payload_message TEXT NOT NULL,
                payload_deliver INTEGER NOT NULL,
                payload_channel TEXT,
                payload_to TEXT,
                next_run_at_ms INTEGER,
                last_run_at_ms INTEGER,
                last_status TEXT,
                last_error TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                delete_after_run INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS cron_runs (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                run_at_ms INTEGER NOT NULL,
                status TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                error TEXT,
                FOREIGN KEY(job_id) REFERENCES cron_jobs(id)
            );
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_cron_jobs_next_run ON cron_jobs(next_run_at_ms);",
        )
        .execute(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_cron_runs_job ON cron_runs(job_id, run_at_ms DESC);",
        )
        .execute(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;

        Ok(())
    }

    pub async fn start(&self) -> ZeroBotResult<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let this = self.clone();
        let mut guard = self.timer_task.lock().await;
        *guard = Some(tokio::spawn(async move {
            while this.running.load(Ordering::SeqCst) {
                if let Err(err) = this.run_due_once().await {
                    tracing::warn!("cron tick error: {}", err);
                }
                sleep(Duration::from_millis(500)).await;
            }
        }));
        Ok(())
    }

    pub async fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let mut guard = self.timer_task.lock().await;
        if let Some(task) = guard.take() {
            task.abort();
        }
    }

    pub async fn status(&self) -> ZeroBotResult<CronServiceStatus> {
        let jobs = self.list_jobs(true).await?;
        let next_wake_at_ms = jobs
            .iter()
            .filter(|j| j.enabled)
            .filter_map(|j| j.state.next_run_at_ms)
            .min();
        Ok(CronServiceStatus {
            enabled: self.running.load(Ordering::SeqCst),
            jobs: jobs.len(),
            next_wake_at_ms,
        })
    }

    pub async fn add_job(
        &self,
        name: String,
        schedule: CronSchedule,
        payload: CronPayload,
        delete_after_run: bool,
    ) -> ZeroBotResult<CronJob> {
        validate_schedule_for_add(&schedule)?;
        let now = now_ms();
        let next_run_at_ms = compute_next_run(&schedule, now)?;
        let id = Uuid::new_v4().to_string()[..8].to_string();

        sqlx::query(
            r#"
            INSERT INTO cron_jobs (
                id, name, enabled, schedule_kind, schedule_at_ms, schedule_every_ms, schedule_expr, schedule_tz,
                payload_kind, payload_message, payload_deliver, payload_channel, payload_to,
                next_run_at_ms, last_run_at_ms, last_status, last_error,
                created_at_ms, updated_at_ms, delete_after_run
            ) VALUES (?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(&name)
        .bind(schedule.kind.as_str())
        .bind(schedule.at_ms)
        .bind(schedule.every_ms)
        .bind(schedule.expr.as_deref())
        .bind(schedule.tz.as_deref())
        .bind(&payload.kind)
        .bind(&payload.message)
        .bind(if payload.deliver { 1 } else { 0 })
        .bind(payload.channel.as_deref())
        .bind(payload.to.as_deref())
        .bind(next_run_at_ms)
        .bind(now)
        .bind(now)
        .bind(if delete_after_run { 1 } else { 0 })
        .execute(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;

        self.export_snapshot().await?;
        self.get_job(&id)
            .await?
            .ok_or_else(|| ZeroBotError::SessionStore("新增任务后无法读取".to_string()))
    }

    pub async fn remove_job(&self, job_id: &str) -> ZeroBotResult<bool> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        let affected = sqlx::query("DELETE FROM cron_jobs WHERE id = ?")
            .bind(job_id)
            .execute(&mut *tx)
            .await
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?
            .rows_affected();
        let _ = sqlx::query("DELETE FROM cron_runs WHERE job_id = ?")
            .bind(job_id)
            .execute(&mut *tx)
            .await;
        tx.commit()
            .await
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        if affected > 0 {
            self.export_snapshot().await?;
        }
        Ok(affected > 0)
    }

    pub async fn enable_job(&self, job_id: &str, enabled: bool) -> ZeroBotResult<Option<CronJob>> {
        let Some(job) = self.get_job(job_id).await? else {
            return Ok(None);
        };
        let now = now_ms();
        let next_run = if enabled {
            compute_next_run(&job.schedule, now)?
        } else {
            None
        };
        sqlx::query(
            "UPDATE cron_jobs SET enabled = ?, next_run_at_ms = ?, updated_at_ms = ? WHERE id = ?",
        )
        .bind(if enabled { 1 } else { 0 })
        .bind(next_run)
        .bind(now)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        self.export_snapshot().await?;
        self.get_job(job_id).await
    }

    pub async fn run_job(&self, job_id: &str, force: bool) -> ZeroBotResult<bool> {
        let Some(job) = self.get_job(job_id).await? else {
            return Ok(false);
        };
        if !force && !job.enabled {
            return Ok(false);
        }
        self.execute_job(job).await?;
        Ok(true)
    }

    pub async fn list_jobs(&self, include_disabled: bool) -> ZeroBotResult<Vec<CronJob>> {
        let query = if include_disabled {
            "SELECT * FROM cron_jobs ORDER BY COALESCE(next_run_at_ms, 9223372036854775807) ASC"
        } else {
            "SELECT * FROM cron_jobs WHERE enabled = 1 ORDER BY COALESCE(next_run_at_ms, 9223372036854775807) ASC"
        };
        let rows = sqlx::query(query)
            .fetch_all(&self.pool)
            .await
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(self.parse_job_row(row).await?);
        }
        Ok(out)
    }

    pub async fn get_job(&self, job_id: &str) -> ZeroBotResult<Option<CronJob>> {
        let row = sqlx::query("SELECT * FROM cron_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        match row {
            Some(row) => Ok(Some(self.parse_job_row(row).await?)),
            None => Ok(None),
        }
    }

    pub async fn export_snapshot(&self) -> ZeroBotResult<Option<PathBuf>> {
        let Some(path) = self.export_path.clone() else {
            return Ok(None);
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        }
        let snapshot = CronStoreSnapshot {
            version: 1,
            jobs: self.list_jobs(true).await?,
        };
        let raw = serde_json::to_string_pretty(&snapshot)
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        tokio::fs::write(&path, raw)
            .await
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        Ok(Some(path))
    }

    async fn run_due_once(&self) -> ZeroBotResult<()> {
        let now = now_ms();
        let rows = sqlx::query(
            "SELECT id FROM cron_jobs WHERE enabled = 1 AND next_run_at_ms IS NOT NULL AND next_run_at_ms <= ? ORDER BY next_run_at_ms ASC",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;

        for row in rows {
            let id: String = row
                .try_get("id")
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
            if let Some(job) = self.get_job(&id).await? {
                self.execute_job(job).await?;
            }
        }
        Ok(())
    }

    async fn execute_job(&self, job: CronJob) -> ZeroBotResult<()> {
        let _guard = self.execution_lock.lock().await;
        let start = now_ms();

        let handler = self.handler.read().await.clone();
        let (status, error) = if let Some(handler) = handler {
            match handler(job.clone()).await {
                Ok(_) => (CronRunStatus::Ok, None),
                Err(err) => (CronRunStatus::Error, Some(err.to_string())),
            }
        } else {
            (CronRunStatus::Skipped, Some("未配置 cron 回调".to_string()))
        };

        let end = now_ms();
        let duration = end.saturating_sub(start);

        sqlx::query(
            "INSERT INTO cron_runs (id, job_id, run_at_ms, status, duration_ms, error) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&job.id)
        .bind(start)
        .bind(status.as_str())
        .bind(duration)
        .bind(error.as_deref())
        .execute(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;

        let now = now_ms();
        let mut enabled = job.enabled;
        let next_run_at_ms;

        if matches!(job.schedule.kind, CronScheduleKind::At) {
            if job.delete_after_run {
                let _ = self.remove_job(&job.id).await?;
                self.trim_runs(&job.id).await?;
                self.export_snapshot().await?;
                return Ok(());
            }
            enabled = false;
            next_run_at_ms = None;
        } else {
            next_run_at_ms = compute_next_run(&job.schedule, now)?;
        }

        sqlx::query(
            "UPDATE cron_jobs SET enabled = ?, next_run_at_ms = ?, last_run_at_ms = ?, last_status = ?, last_error = ?, updated_at_ms = ? WHERE id = ?",
        )
        .bind(if enabled { 1 } else { 0 })
        .bind(next_run_at_ms)
        .bind(start)
        .bind(status.as_str())
        .bind(error.as_deref())
        .bind(now)
        .bind(&job.id)
        .execute(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;

        self.trim_runs(&job.id).await?;
        self.export_snapshot().await?;
        Ok(())
    }

    async fn trim_runs(&self, job_id: &str) -> ZeroBotResult<()> {
        let offset = i64::try_from(self.run_history_limit).unwrap_or(i64::MAX);
        sqlx::query(
            "DELETE FROM cron_runs WHERE job_id = ? AND id IN (SELECT id FROM cron_runs WHERE job_id = ? ORDER BY run_at_ms DESC LIMIT -1 OFFSET ?)",
        )
        .bind(job_id)
        .bind(job_id)
        .bind(offset)
        .execute(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        Ok(())
    }

    async fn parse_job_row(&self, row: sqlx::sqlite::SqliteRow) -> ZeroBotResult<CronJob> {
        let id: String = row
            .try_get("id")
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        let schedule_kind_raw: String = row
            .try_get("schedule_kind")
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
        let schedule_kind = CronScheduleKind::from_str(&schedule_kind_raw).ok_or_else(|| {
            ZeroBotError::SessionStore(format!("无效 schedule_kind: {schedule_kind_raw}"))
        })?;

        let last_status = row
            .try_get::<Option<String>, _>("last_status")
            .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?
            .and_then(|value| CronRunStatus::from_str(&value));

        let run_rows = sqlx::query(
            "SELECT run_at_ms, status, duration_ms, error FROM cron_runs WHERE job_id = ? ORDER BY run_at_ms DESC LIMIT ?",
        )
        .bind(&id)
        .bind(i64::try_from(self.run_history_limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;

        let mut run_history = Vec::new();
        for run in run_rows {
            let status_raw: String = run
                .try_get("status")
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?;
            if let Some(status) = CronRunStatus::from_str(&status_raw) {
                run_history.push(CronRunRecord {
                    run_at_ms: run
                        .try_get("run_at_ms")
                        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                    status,
                    duration_ms: run
                        .try_get("duration_ms")
                        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                    error: run
                        .try_get("error")
                        .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                });
            }
        }

        Ok(CronJob {
            id,
            name: row
                .try_get("name")
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
            enabled: row
                .try_get::<i64, _>("enabled")
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?
                != 0,
            schedule: CronSchedule {
                kind: schedule_kind,
                at_ms: row
                    .try_get("schedule_at_ms")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                every_ms: row
                    .try_get("schedule_every_ms")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                expr: row
                    .try_get("schedule_expr")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                tz: row
                    .try_get("schedule_tz")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
            },
            payload: CronPayload {
                kind: row
                    .try_get("payload_kind")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                message: row
                    .try_get("payload_message")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                deliver: row
                    .try_get::<i64, _>("payload_deliver")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?
                    != 0,
                channel: row
                    .try_get("payload_channel")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                to: row
                    .try_get("payload_to")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
            },
            state: CronJobState {
                next_run_at_ms: row
                    .try_get("next_run_at_ms")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                last_run_at_ms: row
                    .try_get("last_run_at_ms")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                last_status,
                last_error: row
                    .try_get("last_error")
                    .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
                run_history,
            },
            created_at_ms: row
                .try_get("created_at_ms")
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
            updated_at_ms: row
                .try_get("updated_at_ms")
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?,
            delete_after_run: row
                .try_get::<i64, _>("delete_after_run")
                .map_err(|err| ZeroBotError::SessionStore(err.to_string()))?
                != 0,
        })
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub fn compute_next_run(schedule: &CronSchedule, now_ms: i64) -> ZeroBotResult<Option<i64>> {
    match schedule.kind {
        CronScheduleKind::At => Ok(schedule.at_ms.filter(|at| *at > now_ms)),
        CronScheduleKind::Every => {
            let interval = schedule.every_ms.unwrap_or_default();
            if interval <= 0 {
                return Ok(None);
            }
            Ok(Some(now_ms.saturating_add(interval)))
        }
        CronScheduleKind::Cron => {
            let expr = schedule
                .expr
                .as_deref()
                .ok_or_else(|| ZeroBotError::Tool("cron schedule 缺少 expr".to_string()))?;
            let parsed = Schedule::from_str(expr)
                .map_err(|err| ZeroBotError::Tool(format!("无效 cron 表达式: {err}")))?;

            let base_utc = Utc
                .timestamp_millis_opt(now_ms)
                .single()
                .ok_or_else(|| ZeroBotError::Tool("无效时间戳".to_string()))?;

            if let Some(tz_raw) = schedule_tz(schedule_tz_value(schedule)) {
                let base = base_utc.with_timezone(&tz_raw);
                let next = parsed.after(&base).next().map(|dt| dt.timestamp_millis());
                return Ok(next);
            }

            Ok(parsed
                .after(&base_utc)
                .next()
                .map(|dt| dt.timestamp_millis()))
        }
    }
}

fn schedule_tz_value(schedule: &CronSchedule) -> Option<&str> {
    schedule.tz.as_deref().filter(|s| !s.trim().is_empty())
}

fn schedule_tz(raw: Option<&str>) -> Option<Tz> {
    raw.and_then(|s| s.parse::<Tz>().ok())
}

pub fn validate_schedule_for_add(schedule: &CronSchedule) -> ZeroBotResult<()> {
    if schedule.tz.is_some() && !matches!(schedule.kind, CronScheduleKind::Cron) {
        return Err(ZeroBotError::Tool(
            "tz 仅可用于 cron 表达式任务".to_string(),
        ));
    }

    match schedule.kind {
        CronScheduleKind::At => {
            if schedule.at_ms.is_none() {
                return Err(ZeroBotError::Tool("at 任务缺少 at_ms".to_string()));
            }
        }
        CronScheduleKind::Every => {
            if schedule.every_ms.unwrap_or_default() <= 0 {
                return Err(ZeroBotError::Tool(
                    "every 任务需要 every_ms > 0".to_string(),
                ));
            }
        }
        CronScheduleKind::Cron => {
            let expr = schedule
                .expr
                .as_deref()
                .ok_or_else(|| ZeroBotError::Tool("cron 任务缺少 expr".to_string()))?;
            let _ = Schedule::from_str(expr)
                .map_err(|err| ZeroBotError::Tool(format!("无效 cron 表达式: {err}")))?;
            if let Some(tz_raw) = schedule_tz_value(schedule) {
                let _ = tz_raw
                    .parse::<Tz>()
                    .map_err(|_| ZeroBotError::Tool(format!("unknown timezone '{tz_raw}'")))?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_every_schedule() {
        let now = 1000_i64;
        let schedule = CronSchedule::every(500);
        assert_eq!(compute_next_run(&schedule, now).unwrap(), Some(1500));
    }

    #[test]
    fn reject_bad_tz() {
        let schedule = CronSchedule::cron("0 0 9 * * * *", Some("America/Vancovuer".to_string()));
        let err = validate_schedule_for_add(&schedule)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown timezone"));
    }
}
