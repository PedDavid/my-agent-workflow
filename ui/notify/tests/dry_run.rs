mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::Mock;
use drove_client::Client;
use drove_notify::app::{App, Engine, SocketFocuser, UiAction};
use drove_notify::backend::DryRun;
use drove_notify::config::Config;

struct Notifier {
    child: Child,
    lines: mpsc::Receiver<serde_json::Value>,
}

impl Notifier {
    fn start(mock: &Mock, extra: &[&str]) -> Notifier {
        let mut child = Command::new(env!("CARGO_BIN_EXE_drove-notify"))
            .arg("--dry-run")
            .arg("--socket")
            .arg(&mock.socket)
            .args(extra)
            .env("XDG_RUNTIME_DIR", &mock.dir) // no stray quiet file
            .env("XDG_CONFIG_HOME", &mock.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(|l| l.ok()) {
                let v: serde_json::Value = serde_json::from_str(&l).expect("stdout is JSON lines");
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        Notifier { child, lines }
    }

    fn send_line(&mut self, s: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{s}").unwrap();
        stdin.flush().unwrap();
    }
}

impl Drop for Notifier {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn initial_snapshot_is_silent() {
    // Frozen mock: the snapshot already contains a needs_input agent and an
    // idle+attention one, yet nothing may be printed.
    let mock = Mock::start("silent", 0.0);
    let n = Notifier::start(&mock, &[]);
    assert!(
        n.lines.recv_timeout(Duration::from_millis(1500)).is_err(),
        "printed something for the initial snapshot"
    );
}

#[test]
fn notifies_on_change_and_focus_reaches_daemon() {
    let mock = Mock::start("notify", 0.2);
    let mut n = Notifier::start(&mock, &[]);
    let mut first = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if let Ok(v) = n.lines.recv_timeout(Duration::from_millis(200)) {
            if v["op"] == "notify" {
                first = Some(v);
                break;
            }
        }
    }
    let first = first.expect("at least one notification");
    assert_eq!(first["app_name"], "drove");
    let summary = first["summary"].as_str().unwrap();
    assert!(
        summary.starts_with('⚠') || summary.starts_with('✓') || summary.starts_with('×'),
        "{summary}"
    );
    assert!(first["category"].as_str().unwrap().starts_with("x-drove."));

    // Simulate the user pressing "Focus".
    let id = first["agent"].as_str().unwrap().to_string();
    n.send_line(&format!("focus {id}"));
    assert!(
        mock.wait_focus(&id, Duration::from_secs(5)),
        "no focus request for {id}: {:?}",
        mock.requests()
    );
}

#[test]
fn backend_action_focuses_via_client() {
    // Library level: a "Focus" UiAction on the dry-run backend hits the daemon.
    let mock = Mock::start("action", 0.0);
    let agents = Client::connect_to(&mock.socket).unwrap().list().unwrap();
    let target = agents.iter().find(|a| a.name == "api-refactor").unwrap();

    let cfg = Config::default();
    let mut app = App::new(
        Engine::new(cfg.clone(), None),
        DryRun::new(Vec::new(), &cfg),
        SocketFocuser {
            socket: Some(mock.socket.clone()),
        },
    );
    app.on_ui(UiAction::Focus(target.id.clone()));
    assert_eq!(mock.focus_requests(), vec![target.id.clone()]);
    // Dismiss must not focus.
    app.on_ui(UiAction::Dismiss(target.id.clone()));
    assert_eq!(mock.focus_requests().len(), 1);
}
