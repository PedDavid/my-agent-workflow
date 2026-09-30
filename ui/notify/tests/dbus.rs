//! Real D-Bus round trip against a private `dbus-daemon` and a fake
//! `org.freedesktop.Notifications` server. Skipped when dbus-daemon is missing.

mod common;

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use common::Mock;
use zbus::zvariant::OwnedValue;

#[derive(Debug)]
struct Shown {
    app_name: String,
    replaces_id: u32,
    id: u32,
    summary: String,
    actions: Vec<String>,
    hints: HashMap<String, OwnedValue>,
}

struct Fake {
    tx: Sender<Shown>,
    closed: Sender<u32>,
    next: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Fake {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        _body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next.fetch_add(1, Ordering::SeqCst)
        };
        let _ = self.tx.send(Shown {
            app_name,
            replaces_id,
            id,
            summary,
            actions,
            hints,
        });
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.closed.send(id);
    }

    fn get_capabilities(&self) -> Vec<String> {
        vec!["actions".into(), "body".into()]
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("fake".into(), "test".into(), "1".into(), "1.2".into())
    }
}

struct Bus(Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_bus() -> Option<(Bus, String)> {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut addr = String::new();
    BufReader::new(child.stdout.take()?)
        .read_line(&mut addr)
        .ok()?;
    let addr = addr.trim().to_string();
    let bus = Bus(child);
    (!addr.is_empty()).then_some((bus, addr))
}

struct Notifier(Child);
impl Drop for Notifier {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn dbus_notification_and_focus_action() {
    let Some((_bus, addr)) = start_bus() else {
        eprintln!("SKIPPED: dbus-daemon not available");
        return;
    };

    let (tx, shown) = mpsc::channel();
    let (closed_tx, _closed) = mpsc::channel();
    let server = zbus::blocking::connection::Builder::address(addr.as_str())
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at(
            "/org/freedesktop/Notifications",
            Fake {
                tx,
                closed: closed_tx,
                next: AtomicU32::new(1),
            },
        )
        .unwrap()
        .build()
        .unwrap();

    let mock = Mock::start("dbus", 0.2);
    let _n = Notifier(
        Command::new(env!("CARGO_BIN_EXE_drove-notify"))
            .arg("--socket")
            .arg(&mock.socket)
            .env("DBUS_SESSION_BUS_ADDRESS", &addr)
            .env("XDG_RUNTIME_DIR", &mock.dir)
            .env("XDG_CONFIG_HOME", &mock.dir)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let n = shown
        .recv_timeout(Duration::from_secs(20))
        .expect("a notification reached the fake server");
    assert_eq!(n.app_name, "drove");
    assert_eq!(n.replaces_id, 0);
    assert_eq!(n.actions, ["default", "Focus", "dismiss", "Dismiss"]);
    assert!(
        n.summary.starts_with('⚠') || n.summary.starts_with('✓') || n.summary.starts_with('×'),
        "{}",
        n.summary
    );
    let cat: String = n.hints["category"].clone().try_into().unwrap();
    assert!(cat.starts_with("x-drove."), "{cat}");
    let entry: String = n.hints["desktop-entry"].clone().try_into().unwrap();
    assert_eq!(entry, "drove");
    assert!(n.hints.contains_key("urgency"));

    // Give the notifier a moment to subscribe to signals, then click "Focus".
    std::thread::sleep(Duration::from_millis(500));
    server
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "ActionInvoked",
            &(n.id, "default"),
        )
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while mock.focus_requests().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "no focus request after ActionInvoked"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
