use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Mock(Child);
impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_mock(dir: &Path, tick: &str) -> Mock {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/mock-drove.py");
    let child = Command::new("python3")
        .arg(script)
        .arg("--socket")
        .arg(dir.join("d.sock"))
        .args(["--tick", tick, "--log"])
        .arg(dir.join("req.log"))
        .stdout(Stdio::null())
        .spawn()
        .expect("python3 available");
    for _ in 0..100 {
        if dir.join("d.sock").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Mock(child)
}

async fn start_web(dir: &Path) -> SocketAddr {
    let app = drove_web::App::new(dir.join("d.sock"));
    if dir.join("d.sock").exists() {
        for _ in 0..100 {
            if app.is_up() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, drove_web::router(app)).await.unwrap() });
    addr
}

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    extra: &str,
    body: &str,
) -> (u16, String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.unwrap();
    let status = buf.split(' ').nth(1).unwrap().parse().unwrap();
    let body = buf
        .split_once("\r\n\r\n")
        .map(|x| x.1)
        .unwrap_or("")
        .to_string();
    (status, body)
}

fn json(body: &str) -> Value {
    serde_json::from_str(body.trim()).unwrap_or_else(|e| panic!("bad json {body:?}: {e}"))
}

fn log_lines(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("req.log"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn lists_agents_and_serves_page() {
    let dir = tempfile::tempdir().unwrap();
    let _m = start_mock(dir.path(), "0");
    let addr = start_web(dir.path()).await;
    let (st, body) = http(addr, "GET", "/api/agents", "", "").await;
    assert_eq!(st, 200);
    let v = json(&body);
    let names: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 4);
    assert_eq!(names[0], "fix-flaky-tests"); // needs_input sorts first
    for n in ["api-refactor", "infra-docs", "scratch"] {
        assert!(names.contains(&n));
    }
    let (st, page) = http(addr, "GET", "/", "", "").await;
    assert_eq!(st, 200);
    assert!(page.contains("<title>drove</title>"));
}

#[tokio::test]
async fn post_requires_csrf_header_and_focus_reaches_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let _m = start_mock(dir.path(), "0");
    let addr = start_web(dir.path()).await;
    let (_, body) = http(addr, "GET", "/api/agents", "", "").await;
    let id = json(&body)[0]["id"].as_str().unwrap().to_string();
    let path = format!("/api/agents/{id}/focus");

    let (st, body) = http(addr, "POST", &path, "", "").await;
    assert_eq!(st, 403);
    assert!(json(&body)["error"].is_string());
    assert!(!log_lines(dir.path()).iter().any(|l| l["method"] == "focus"));

    let (st, _) = http(addr, "POST", &path, "X-Drove: 1\r\n", "").await;
    assert_eq!(st, 200);
    let want = serde_json::json!({"method": "focus", "params": {"id": id}});
    assert!(log_lines(dir.path())
        .iter()
        .any(|l| l["method"] == want["method"] && l["params"] == want["params"]));

    // daemon-side errors come back as JSON {error}
    let (st, body) = http(
        addr,
        "POST",
        "/api/agents/nope-zz/focus",
        "X-Drove: 1\r\n",
        "",
    )
    .await;
    assert_eq!(st, 400);
    assert!(json(&body)["error"].as_str().unwrap().contains("no agent"));

    // wrong Host header (DNS rebinding) is refused
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /api/agents HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.unwrap();
    assert!(buf.starts_with("HTTP/1.1 403"));
}

#[tokio::test]
async fn other_actions_text_send_rename_forget() {
    let dir = tempfile::tempdir().unwrap();
    let _m = start_mock(dir.path(), "0");
    let addr = start_web(dir.path()).await;
    let h = "X-Drove: 1\r\n";
    let (st, body) = http(
        addr,
        "GET",
        "/api/agents/scratch/text?extent=screen",
        "",
        "",
    )
    .await;
    assert_eq!(st, 200);
    assert!(json(&body)["text"].as_str().unwrap().contains("scratch"));
    let (st, _) = http(addr, "GET", "/api/agents/scratch/text?extent=bogus", "", "").await;
    assert_eq!(st, 400);
    let (st, _) = http(
        addr,
        "POST",
        "/api/agents/scratch/send",
        h,
        r#"{"text":"hi","enter":true}"#,
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = http(
        addr,
        "POST",
        "/api/agents/scratch/rename",
        h,
        r#"{"name":"renamed"}"#,
    )
    .await;
    assert_eq!(st, 200);
    let (st, body) = http(addr, "POST", "/api/next", h, "").await;
    assert_eq!(st, 200);
    assert!(json(&body).get("agent").is_some());
    let (st, _) = http(addr, "POST", "/api/agents/renamed/close", h, "").await;
    assert_eq!(st, 200);
    let (st, body) = http(addr, "POST", "/api/forget-exited", h, "").await;
    assert_eq!(st, 200);
    assert_eq!(json(&body)["removed"], 1);
}

/// Read SSE from `/events` until `done(events)` or timeout; returns (name, data) pairs.
async fn sse_until(
    addr: SocketAddr,
    done: impl Fn(&[(String, Value)]) -> bool,
) -> Vec<(String, Value)> {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(
        format!("GET /events HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    let mut buf = String::new();
    let mut out = vec![];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut chunk = [0u8; 4096];
    while !done(&out) && tokio::time::Instant::now() < deadline {
        let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(500), s.read(&mut chunk)).await
        else {
            continue;
        };
        if n == 0 {
            break;
        }
        buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
        out = buf
            .split("\n\n")
            .filter_map(|blk| {
                let name = blk
                    .lines()
                    .find_map(|l| l.strip_prefix("event:"))?
                    .trim()
                    .to_string();
                let data = blk.lines().find_map(|l| l.strip_prefix("data:"))?.trim();
                Some((name, serde_json::from_str(data).ok()?))
            })
            .collect();
    }
    out
}

#[tokio::test]
async fn events_stream_snapshot_then_agent_events() {
    let dir = tempfile::tempdir().unwrap();
    let _m = start_mock(dir.path(), "0.2");
    let addr = start_web(dir.path()).await;
    let ev = sse_until(addr, |e| e.iter().filter(|x| x.0 == "agent").count() >= 2).await;
    let names: Vec<_> = ev.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names[0], "daemon");
    assert_eq!(ev[0].1["up"], true);
    assert_eq!(names[1], "snapshot");
    assert_eq!(ev[1].1["agents"].as_array().unwrap().len(), 4);
    assert!(
        names.iter().filter(|n| **n == "agent").count() >= 2,
        "{names:?}"
    );
    assert!(ev
        .iter()
        .any(|e| e.0 == "agent" && e.1["agent"]["id"].is_string()));
}

#[tokio::test]
async fn reports_daemon_down_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let addr = start_web(dir.path()).await; // no daemon yet
    let (st, body) = http(addr, "GET", "/api/agents", "", "").await;
    assert_eq!(st, 503);
    assert!(json(&body)["error"].is_string());
    let ev = sse_until(addr, |e| !e.is_empty()).await;
    assert_eq!(ev[0].0, "daemon");
    assert_eq!(ev[0].1["up"], false);

    let _m = start_mock(dir.path(), "0");
    let ev = sse_until(addr, |e| e.iter().any(|x| x.0 == "snapshot")).await;
    assert!(ev.iter().any(|e| e.0 == "snapshot"), "{ev:?}");
}

#[test]
fn listen_guard() {
    let lo: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let any: SocketAddr = "0.0.0.0:1".parse().unwrap();
    assert!(drove_web::check_listen(&lo, false).is_ok());
    assert!(drove_web::check_listen(&any, false).is_err());
    assert!(drove_web::check_listen(&any, true).is_ok());
}
