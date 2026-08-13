//! `cos` — the Chief of Staff daemon.
//!
//! Wraps `agentcal`'s calendar + CRM façades behind a tiny HTTP/JSON server
//! over loopback. Designed to run on the phone via Termux and be reached by
//! the agent through `adb forward`.
//!
//! Usage:
//!   cos serve --addr 127.0.0.1:8788 --db .agentcal/cos.db
//!   cos seed  --db .agentcal/cos.db
//!
//! Protocol: `POST /` with a JSON body `{"method": "...", "params": {...}}`
//! → `{"ok": true, "result": ...}` or `{"ok": false, "error": "..."}`.
//! GET /ping → `{"ok": true, "result": {"pong": true}}`.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;

use agentcal::rpc::dispatch;
use agentcal::{AgentCal, AgentCrm, LibSqlStore};

mod aware;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: cos <serve|seed> [--addr HOST:PORT] [--db PATH]");
        std::process::exit(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut addr = "127.0.0.1:8788".to_string();
    let mut db = ".agentcal/cos.db".to_string();

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => {
                i += 1;
                addr = args.get(i).expect("addr value").clone();
            }
            "--db" => {
                i += 1;
                db = args.get(i).expect("db value").clone();
            }
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    match args[1].as_str() {
        "serve" => rt.block_on(serve(&addr, &db)),
        "seed" => rt.block_on(seed(&db)),
        other => {
            eprintln!("unknown subcommand: {other}");
            std::process::exit(2);
        }
    }
}

/// Idempotently seed the DB, print the resulting summary.
async fn seed(db: &str) {
    match LibSqlStore::open(PathBuf::from(db)).await {
        Ok(store) => {
            let crm = AgentCrm::new(store);
            if let Err(e) = agentcal::seed::seed_cos(&crm, "derrick").await {
                eprintln!("seed error: {e}");
                std::process::exit(1);
            }
            match crm.summary("derrick").await {
                Ok(s) => println!("seeded {db}: {s:?}"),
                Err(e) => eprintln!("summary error: {e}"),
            }
        }
        Err(e) => {
            eprintln!("cannot open db {db}: {e}");
            std::process::exit(1);
        }
    }
}

/// Run the HTTP daemon until killed.
async fn serve(addr: &str, db: &str) {
    let store = match LibSqlStore::open(PathBuf::from(db)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open db {db}: {e}");
            std::process::exit(1);
        }
    };
    let cal = Arc::new(AgentCal::new(store.clone()));
    let crm = Arc::new(AgentCrm::new(store));

    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    println!("cos listening on http://{addr} (db={db})");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let cal = cal.clone();
                let crm = crm.clone();
                std::thread::spawn(move || {
                    // Each connection gets its own Tokio runtime so the
                    // async libSQL calls in `dispatch` have a reactor.
                    let rt = tokio::runtime::Runtime::new().expect("connection runtime");
                    if let Err(e) = rt.block_on(handle(stream, &cal, &crm)) {
                        eprintln!("handler error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

/// One connection: read request, dispatch, respond.
async fn handle(
    mut stream: std::net::TcpStream,
    cal: &AgentCal,
    crm: &AgentCrm,
) -> Result<(), Box<dyn std::error::Error>> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        // Stop once we've seen the full headers + body (Content-Length matched).
        if let Some(body_end) = headers_and_body_len(&buf) {
            if buf.len() >= body_end {
                break;
            }
        }
    }

    let request = String::from_utf8_lossy(&buf);
    let response = route(&request, cal, crm).await;
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    stream.shutdown(Shutdown::Write)?;
    Ok(())
}

/// Find the byte offset just past headers+body given what we've read so far.
fn headers_and_body_len(buf: &[u8]) -> Option<usize> {
    let header_end = find_header_end(buf)?;
    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut content_length = 0usize;
    for line in head.lines() {
        if line.to_ascii_lowercase().starts_with("content-length:") {
            if let Some(v) = line.split(':').nth(1) {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    Some(header_end + content_length)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

async fn route(request: &str, cal: &AgentCal, crm: &AgentCrm) -> String {
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    // GET /ping — cheap health check.
    if method == "GET" && (path == "/ping" || path == "/healthz") {
        return http_json(200, &json_ok(serde_json::json!({"pong": true})));
    }

    if method != "POST" {
        return http_json(405, &json_err("only POST is supported"));
    }

    let body = request
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();

    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return http_json(400, &json_err(&format!("bad JSON body: {e}"))),
    };
    let method_name = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = v.get("params").cloned().unwrap_or(serde_json::Value::Null);

    // Situational-awareness methods (cos brain → informed phone action).
    let result = match method_name {
        "aware.sms" => crate::aware::aware_sms(crm, &params).await,
        "aware.whatsapp" => crate::aware::aware_whatsapp(crm, &params).await,
        "aware.call" => crate::aware::aware_call(crm, &params).await,
        "aware.capture" => crate::aware::aware_capture(crm, &params).await,
        "aware.sync_contacts" => crate::aware::sync_contacts(crm, &params).await,
        "aware.travel" => crate::aware::aware_travel(crm, &params).await,
        "aware.meeting" => crate::aware::aware_meeting(cal, crm, &params).await,
        "aware.briefing" => crate::aware::aware_briefing(cal, crm, &params).await,
        "aware.deals" => crate::aware::aware_deals(crm, &params).await,
        _ => dispatch(cal, crm, method_name, &params).await,
    };
    match result {
        Ok(value) => http_json(200, &json_ok(value)),
        Err(e) => http_json(200, &json_err(&e.to_string())),
    }
}

/// Wrap a JSON body in a proper HTTP/1.1 response.
fn http_json(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn json_ok(result: serde_json::Value) -> String {
    serde_json::json!({ "ok": true, "result": result }).to_string()
}

fn json_err(error: &str) -> String {
    serde_json::json!({ "ok": false, "error": error }).to_string()
}
