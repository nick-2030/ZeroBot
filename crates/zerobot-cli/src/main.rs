use std::io::{self, BufRead, Write};

use clap::{Parser, Subcommand};

use zerobot_sdk::ZerobotClient;

mod tui;

#[derive(Parser)]
#[command(name = "zerobot")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(long, default_value = "http://127.0.0.1:9080")]
    server: String,

    #[arg(long, default_value = "dev-key")]
    api_key: String,
}

#[derive(Subcommand)]
enum Commands {
    Tui,
    Acp,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Tui => tui::run(cli.server, cli.api_key),
        Commands::Acp => run_acp(cli.server, cli.api_key),
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
