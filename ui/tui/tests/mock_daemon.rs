//! Drives the app logic headlessly against tools/mock-drove.py and checks the
//! requests the mock logged.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use drove_client::Client;
use drove_tui::app::{Action, App, Msg};
use drove_tui::worker::{follow_at, run_action, spawn_executor};
use serde_json::Value;

struct Mock {
    child: Child,
    dir: PathBuf,
    sock: PathBuf,
    log: PathBuf,
}

impl Mock {
    fn start(tag: &str) -> Mock {
        let dir = std::env::temp_dir().join(format!("drove-tui-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("m.sock");
        let log = dir.join("req.log");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/mock-drove.py");
        let child = Command::new("python3")
            .arg(script)
            .args(["--socket", sock.to_str().unwrap(), "--tick", "0.2", "--log"])
            .arg(&log)
            .stdout(Stdio::null())
            .spawn()
            .expect("python3 available");
        let t = Instant::now();
        while Client::connect_to(&sock).is_err() {
            assert!(t.elapsed() < Duration::from_secs(10), "mock did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Mock {
            child,
            dir,
            sock,
            log,
        }
    }

    fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

/// Feed the app from the follower until it has agents.
fn app_from_follower(sock: &Path) -> (App, mpsc::Receiver<Msg>) {
    let (tx, rx) = mpsc::channel();
    let p = sock.to_path_buf();
    let (t1, t2, t3) = (tx.clone(), tx.clone(), tx);
    std::thread::spawn(move || {
        follow_at(
            &p,
            move || t1.send(Msg::Connected).is_ok(),
            move |e| t2.send(Msg::Event(e)).is_ok(),
            move |e| t3.send(Msg::Disconnected(e.to_string())).is_ok(),
        )
    });
    let mut app = App::new();
    let t = Instant::now();
    while app.agents.len() < 4 {
        let m = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("snapshot from mock");
        app.handle_msg(m);
        assert!(t.elapsed() < Duration::from_secs(10));
    }
    assert!(app.connected);
    (app, rx)
}

fn find<'a>(reqs: &'a [Value], method: &str) -> Vec<&'a Value> {
    reqs.iter().filter(|r| r["method"] == method).collect()
}

#[test]
fn keys_drive_focus_send_rename_close_forget() {
    let mock = Mock::start("keys");
    let (mut app, _rx) = app_from_follower(&mock.sock);
    let mut client = Client::connect_to(&mock.sock).unwrap();

    // Enter -> focus on the selected (first sorted) agent.
    let id = app.selected().unwrap().id.clone();
    let actions = app.handle_key(key(KeyCode::Enter));
    assert_eq!(actions, vec![Action::Focus(id.clone())]);
    for a in &actions {
        run_action(&mut client, a).unwrap();
    }

    // n -> next
    for a in app.handle_key(key(KeyCode::Char('n'))) {
        run_action(&mut client, &a).unwrap();
    }

    // s -> send prompt with enter=true
    let target = app.selected().unwrap().id.clone();
    app.handle_key(key(KeyCode::Char('s')));
    for c in "run tests".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    for a in app.handle_key(key(KeyCode::Enter)) {
        run_action(&mut client, &a).unwrap();
    }

    // r -> rename (prefilled with the current name; replace it)
    let old = app.selected().unwrap().name.clone();
    app.handle_key(key(KeyCode::Char('r')));
    for _ in 0..old.chars().count() {
        app.handle_key(key(KeyCode::Backspace));
    }
    for c in "renamed".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    for a in app.handle_key(key(KeyCode::Enter)) {
        run_action(&mut client, &a).unwrap();
    }

    // x, y -> close; f -> forget exited
    app.handle_key(key(KeyCode::Char('x')));
    for a in app.handle_key(key(KeyCode::Char('y'))) {
        run_action(&mut client, &a).unwrap();
    }
    for a in app.handle_key(key(KeyCode::Char('f'))) {
        let out = run_action(&mut client, &a).unwrap();
        assert!(out.starts_with("forgot"), "{out}");
    }

    let reqs = mock.requests();
    let f = find(&reqs, "focus");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0]["params"]["id"], id.as_str());
    assert_eq!(find(&reqs, "next").len(), 1);

    let s = find(&reqs, "send_text");
    assert_eq!(s.len(), 1);
    assert_eq!(s[0]["params"]["id"], target.as_str());
    assert_eq!(s[0]["params"]["text"], "run tests");
    assert_eq!(s[0]["params"]["enter"], true);

    let r = find(&reqs, "rename");
    assert_eq!(r[0]["params"]["id"], target.as_str());
    assert_eq!(r[0]["params"]["name"], "renamed");

    let c = find(&reqs, "close");
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["params"]["id"], target.as_str());
    let fg = find(&reqs, "forget");
    assert_eq!(fg.len(), 1);
    assert_eq!(fg[0]["params"]["exited"], true);
}

#[test]
fn follower_sees_changes_and_errors_surface() {
    let mock = Mock::start("events");
    let (mut app, rx) = app_from_follower(&mock.sock);
    let mut client = Client::connect_to(&mock.sock).unwrap();

    // Rename through the client; the follower must deliver the update.
    let id = app.agents[0].id.clone();
    client.rename(&id, "via-client").unwrap();
    let t = Instant::now();
    while !app.agents.iter().any(|a| a.name == "via-client") {
        assert!(t.elapsed() < Duration::from_secs(10), "no agent event");
        if let Ok(m) = rx.recv_timeout(Duration::from_millis(200)) {
            app.handle_msg(m);
        }
    }

    // A daemon error becomes a red status line message.
    let err = run_action(
        &mut client,
        &Action::Focus("definitely-not-an-agent".into()),
    )
    .unwrap_err();
    app.handle_msg(Msg::ActionDone(Err(err)));
    let st = app.status.as_ref().unwrap();
    assert!(st.is_error && st.text.contains("no agent matching"));
}

#[test]
fn executor_thread_reports_results() {
    let mock = Mock::start("exec");
    let (tx, rx) = mpsc::channel();
    let (atx, arx) = mpsc::channel();
    spawn_executor(mock.sock.clone(), arx, tx);

    atx.send(Action::Send {
        id: "api-refactor".into(),
        text: "hello".into(),
    })
    .unwrap();
    match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        Msg::ActionDone(Ok(s)) => assert_eq!(s, "sent"),
        other => panic!("unexpected {other:?}"),
    }
    atx.send(Action::Close("nope-nope".into())).unwrap();
    match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        Msg::ActionDone(Err(e)) => assert!(e.contains("no agent matching"), "{e}"),
        other => panic!("unexpected {other:?}"),
    }
    let reqs = mock.requests();
    assert_eq!(find(&reqs, "send_text")[0]["params"]["enter"], true);
    assert_eq!(find(&reqs, "close").len(), 1);
}

#[test]
fn get_text_screen_for_preview() {
    let mock = Mock::start("preview");
    let mut client = Client::connect_to(&mock.sock).unwrap();
    let a = client.list().unwrap().remove(0);
    let text = client.get_text(&a.id, "screen").unwrap();
    assert!(text.contains(&a.name));
    let reqs = mock.requests();
    let g = find(&reqs, "get_text");
    assert_eq!(g[0]["params"]["extent"], "screen");
}
