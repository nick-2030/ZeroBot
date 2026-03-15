use crate::config::Settings;
use crate::error::{ZeroBotError, ZeroBotResult};
use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};

pub struct LogGuard {
    _file: Arc<Mutex<std::fs::File>>,
}

pub fn init_logging(settings: &Settings, session_id: Option<&str>) -> ZeroBotResult<LogGuard> {
    let level = settings.logging.level.clone();
    let filter = EnvFilter::new(level);
    let session = session_id.unwrap_or("system");
    let log_path = build_log_path(session);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|err| ZeroBotError::Io(err.to_string()))?;
    let file = Arc::new(Mutex::new(file));
    let writer = FileMakeWriter {
        file: Arc::clone(&file),
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer);
    let stdout_layer = tracing_subscriber::fmt::layer().with_ansi(true);

    let _ = Registry::default()
        .with(filter)
        .with(file_layer)
        .with(stdout_layer)
        .try_init();

    Ok(LogGuard { _file: file })
}

fn build_log_path(session_id: &str) -> PathBuf {
    let date = Local::now().format("%Y-%m-%d").to_string();
    let base = log_root();
    base.join(date).join(format!("{session_id}.log"))
}

fn log_root() -> PathBuf {
    expand_home("~/.zerobot/logs")
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

fn home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home);
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        return PathBuf::from(home);
    }
    PathBuf::from(".")
}

struct FileMakeWriter {
    file: Arc<Mutex<std::fs::File>>,
}

impl<'a> MakeWriter<'a> for FileMakeWriter {
    type Writer = FileWriter;

    fn make_writer(&'a self) -> Self::Writer {
        FileWriter {
            file: Arc::clone(&self.file),
        }
    }
}

struct FileWriter {
    file: Arc<Mutex<std::fs::File>>,
}

impl Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut file = self.file.lock().unwrap();
        file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut file = self.file.lock().unwrap();
        file.flush()
    }
}
