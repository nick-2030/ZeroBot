use std::io::{self, BufRead, BufReader, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use serde_json::Value;
use zerobot_sdk::ZerobotClient;

#[derive(Parser)]
#[command(name = "zerobot")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(long, default_value = "http://127.0.0.1:9080")]
    server: String,

    #[arg(long, default_value = "dev-key")]
    api_key: String,
}

#[derive(Subcommand)]
enum Commands {
    Acp,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    ensure_server_running(&cli.server)?;
    match cli.command {
        Some(Commands::Acp) => run_acp(cli.server, cli.api_key),
        None => run_chat(cli.server, cli.api_key),
    }
}

#[derive(serde::Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: serde_json::Value,
    method: String,
    params: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(serde::Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

fn run_acp(server: String, api_key: String) -> anyhow::Result<()> {
    let client = ZerobotClient::new(server, api_key);
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Result<RpcRequest, _> = serde_json::from_str(&line);
        let response = match request {
            Ok(req) => handle_rpc(&client, req),
            Err(err) => RpcResponse {
                jsonrpc: "2.0",
                id: serde_json::Value::Null,
                result: None,
                error: Some(RpcError { code: -32700, message: err.to_string() }),
            },
        };
        let payload = serde_json::to_string(&response)?;
        writeln!(stdout, "{}", payload)?;
        stdout.flush()?;
    }
    Ok(())
}

fn run_chat(server: String, api_key: String) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let client = ZerobotClient::new(server.clone(), api_key.clone());
    let session_id = rt.block_on(client.create_session(Some("CLI Session".to_string())))?;

    let (event_tx, event_rx) = mpsc::channel();
    spawn_sse_thread(server.clone(), api_key.clone(), session_id.clone(), event_tx);

    let mut stdout = io::stdout();
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    loop {
        write_line_user_prefix(&mut stdout)?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let content = line.trim().to_string();
        if content.is_empty() {
            continue;
        }
        if let Err(err) = rt.block_on(client.send_message(&session_id, content)) {
            write_line_error(&mut stdout, &format!("发送失败: {}", err))?;
            continue;
        }
        wait_for_turn(&event_rx, &mut stdout)?;
    }
    Ok(())
}

fn handle_rpc(client: &ZerobotClient, req: RpcRequest) -> RpcResponse {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    match req.method.as_str() {
        "initialize" => RpcResponse {
            jsonrpc: "2.0",
            id: req.id,
            result: Some(serde_json::json!({
                "serverInfo": {"name":"zerobot-acp","version":"0.1.0"},
                "capabilities": {"sessions": true, "tools": true}
            })),
            error: None,
        },
        "session.create" => {
            let title = req
                .params
                .as_ref()
                .and_then(|v| v.get("title"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let result = rt.block_on(client.create_session(title));
            match result {
                Ok(id) => RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(serde_json::json!({"session_id": id})),
                    error: None,
                },
                Err(err) => RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: None,
                    error: Some(RpcError { code: -32000, message: err.to_string() }),
                },
            }
        }
        "session.message" => {
            let content = req
                .params
                .as_ref()
                .and_then(|v| v.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let session_id = req
                .params
                .as_ref()
                .and_then(|v| v.get("session_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let result = rt.block_on(client.send_message(&session_id, content));
            match result {
                Ok(_) => RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(serde_json::json!({"ok": true})),
                    error: None,
                },
                Err(err) => RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: None,
                    error: Some(RpcError { code: -32000, message: err.to_string() }),
                },
            }
        }
        "tools.list" => {
            let result = rt.block_on(client.list_tools());
            match result {
                Ok(tools) => RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(serde_json::json!({"tools": tools})),
                    error: None,
                },
                Err(err) => RpcResponse {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: None,
                    error: Some(RpcError { code: -32000, message: err.to_string() }),
                },
            }
        }
        _ => RpcResponse {
            jsonrpc: "2.0",
            id: req.id,
            result: None,
            error: Some(RpcError { code: -32601, message: "method not found".to_string() }),
        },
    }
}

enum UiEvent {
    Sse { event_type: String, data: Value },
}

fn wait_for_turn(rx: &Receiver<UiEvent>, stdout: &mut io::Stdout) -> anyhow::Result<()> {
    let mut output = String::new();
    let mut assistant_started = false;
    let mut assistant_line_open = false;
    let mut can_rewrite = true;
    loop {
        let event = rx.recv().map_err(|_| anyhow::anyhow!("事件流已断开"))?;
        match event {
            UiEvent::Sse { event_type, data } => match event_type.as_str() {
                "token" => {
                    let chunk = data
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !assistant_started {
                        render_pending(stdout)?;
                        assistant_started = true;
                        assistant_line_open = true;
                    }
                    if !chunk.is_empty() {
                        write!(stdout, "{}", chunk)?;
                        stdout.flush()?;
                    }
                    output.push_str(chunk);
                }
                "tool_call" => {
                    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                    let args = data.get("arguments").cloned().unwrap_or(Value::Null);
                    if assistant_line_open {
                        write!(stdout, "\n")?;
                        stdout.flush()?;
                        assistant_line_open = false;
                        can_rewrite = false;
                    }
                    write_line_tool_pending(stdout, name, &args)?;
                }
                "tool_result" => {
                    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                    let out = data.get("output").cloned().unwrap_or(Value::Null);
                    let is_error = data.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                    if assistant_line_open {
                        write!(stdout, "\n")?;
                        stdout.flush()?;
                        assistant_line_open = false;
                        can_rewrite = false;
                    }
                    write_line_tool_result(stdout, name, &out, !is_error)?;
                }
                "agent_status" => {
                    let state = data.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    match state {
                        "completed" => {
                            if assistant_started && can_rewrite {
                                render_final(stdout, &output, true)?;
                            } else if assistant_started {
                                write_final_line(stdout, "", true)?;
                            } else if output.trim().is_empty() {
                                write_final_line(stdout, "", true)?;
                            } else {
                                write_final_line(stdout, &output, true)?;
                            }
                            return Ok(());
                        }
                        "failed" => {
                            let err = data.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
                            let mut msg = output.clone();
                            if msg.trim().is_empty() {
                                msg = err.to_string();
                            } else {
                                msg.push_str(" ");
                                msg.push_str(err);
                            }
                            if assistant_started && can_rewrite {
                                render_final(stdout, &msg, false)?;
                            } else {
                                write_final_line(stdout, &msg, false)?;
                            }
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
        }
    }
}

fn spawn_sse_thread(server: String, api_key: String, session_id: String, tx: Sender<UiEvent>) {
    thread::spawn(move || loop {
        let url = format!("{}/v1/sessions/{}/events", server.trim_end_matches('/'), session_id);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(url)
            .header("x-zerobot-api-key", api_key.clone())
            .send();
        let resp = match resp {
            Ok(resp) => resp,
            Err(_) => {
                thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let mut event_type = String::new();
        let mut data_buf = String::new();
        let reader = BufReader::new(resp);
        for line in reader.lines().flatten() {
            let line = line.trim_end();
            if line.is_empty() {
                if !data_buf.is_empty() {
                    let data = serde_json::from_str::<Value>(&data_buf)
                        .unwrap_or_else(|_| Value::String(data_buf.clone()));
                    let evt = UiEvent::Sse {
                        event_type: if event_type.is_empty() {
                            "message".to_string()
                        } else {
                            event_type.clone()
                        },
                        data,
                    };
                    let _ = tx.send(evt);
                    event_type.clear();
                    data_buf.clear();
                }
                continue;
            }
            if let Some(value) = line.strip_prefix("event:") {
                event_type = value.trim().to_string();
                continue;
            }
            if let Some(value) = line.strip_prefix("data:") {
                if !data_buf.is_empty() {
                    data_buf.push('\n');
                }
                data_buf.push_str(value.trim());
            }
        }
        thread::sleep(Duration::from_millis(300));
    });
}

fn write_line_user_prefix(stdout: &mut io::Stdout) -> anyhow::Result<()> {
    write!(stdout, "\x1b[36m> \x1b[0m\n")?;
    stdout.flush()?;
    Ok(())
}

fn write_line_error(stdout: &mut io::Stdout, content: &str) -> anyhow::Result<()> {
    let line = sanitize_output(content);
    write!(stdout, "\x1b[31m● \x1b[0m{}\n", line)?;
    stdout.flush()?;
    Ok(())
}

fn write_line_tool_pending(stdout: &mut io::Stdout, name: &str, args: &Value) -> anyhow::Result<()> {
    let payload = sanitize_output(&format!("[tool:{}] {}", name, args));
    write!(stdout, "\x1b[37m\x1b[5m● \x1b[0m{}\n", payload)?;
    stdout.flush()?;
    Ok(())
}

fn write_line_tool_result(stdout: &mut io::Stdout, name: &str, output: &Value, ok: bool) -> anyhow::Result<()> {
    let payload = sanitize_output(&format!("[tool:{}] {}", name, output));
    let color = if ok { "\x1b[32m" } else { "\x1b[31m" };
    write!(stdout, "{}● \x1b[0m{}\n", color, payload)?;
    stdout.flush()?;
    Ok(())
}

fn render_pending(stdout: &mut io::Stdout) -> anyhow::Result<()> {
    write!(stdout, "\r\x1b[2K\x1b[37m\x1b[5m● \x1b[0m")?;
    stdout.flush()?;
    Ok(())
}

fn render_final(stdout: &mut io::Stdout, content: &str, success: bool) -> anyhow::Result<()> {
    let line = sanitize_output(content);
    let color = if success { "\x1b[32m" } else { "\x1b[31m" };
    write!(stdout, "\r\x1b[2K{}● \x1b[0m{}\n", color, line)?;
    stdout.flush()?;
    Ok(())
}

fn write_final_line(stdout: &mut io::Stdout, content: &str, success: bool) -> anyhow::Result<()> {
    let line = sanitize_output(content);
    let color = if success { "\x1b[32m" } else { "\x1b[31m" };
    write!(stdout, "{}● \x1b[0m{}\n", color, line)?;
    stdout.flush()?;
    Ok(())
}

fn sanitize_output(input: &str) -> String {
    input
        .replace('\r', " ")
        .replace('\n', " ")
        .trim_end()
        .to_string()
}

fn ensure_server_running(server: &str) -> anyhow::Result<()> {
    if is_server_healthy(server) {
        return Ok(());
    }

    let cmd = std::env::var("ZEROBOT_SERVER_CMD")
        .unwrap_or_else(|_| "cargo run -p zerobot-server".to_string());

    let mut child = std::process::Command::new("sh")
        .arg("-lc")
        .arg(&cmd)
        .spawn()
        .map_err(|err| anyhow::anyhow!("failed to start server via '{}': {}", cmd, err))?;

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        if is_server_healthy(server) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let _ = child.try_wait();
    anyhow::bail!("server did not become ready in time (waited 60s)")
}

fn is_server_healthy(server: &str) -> bool {
    let url = format!("{}/health", server.trim_end_matches('/'));
    let Ok(rt) = tokio::runtime::Runtime::new() else {
        return false;
    };
    rt.block_on(async move {
        match reqwest::get(url).await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    })
}
