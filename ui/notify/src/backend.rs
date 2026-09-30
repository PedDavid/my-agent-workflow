//! Backends: freedesktop notifications over D-Bus, and a JSON-lines dry run.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use notify_rust::{Hint, Notification, Timeout, Urgency as XUrgency};
use serde_json::json;

use crate::app::{Backend, UiAction};
use crate::config::Config;
use crate::decide::{Notice, Urgency};

pub const APP_NAME: &str = "drove";

fn spawn_sound(agent_id: &str, command: &str) -> Result<()> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("DROVE_AGENT_ID", agent_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("running sound hook {command:?}"))?;
    // Reap in the background so we never leave zombies.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Prints what would be sent, one JSON object per line.
pub struct DryRun<W: Write> {
    out: W,
    icon: String,
}

impl<W: Write> DryRun<W> {
    pub fn new(out: W, cfg: &Config) -> Self {
        DryRun {
            out,
            icon: cfg.icon.clone(),
        }
    }

    fn line(&mut self, v: serde_json::Value) -> Result<()> {
        writeln!(self.out, "{v}")?;
        self.out.flush()?;
        Ok(())
    }
}

impl<W: Write> Backend for DryRun<W> {
    fn notify(&mut self, n: &Notice) -> Result<()> {
        let icon = self.icon.clone();
        self.line(json!({
            "op": "notify",
            "app_name": APP_NAME,
            "agent": n.agent_id,
            "name": n.name,
            "transition": n.transition,
            "summary": n.summary,
            "body": n.body,
            "urgency": n.urgency,
            "category": n.category,
            "icon": icon,
            "desktop_entry": APP_NAME,
            "actions": ["Focus", "Dismiss"],
        }))
    }

    fn close(&mut self, agent_id: &str) -> Result<()> {
        self.line(json!({"op": "close", "agent": agent_id}))
    }

    fn sound(&mut self, agent_id: &str, command: &str) -> Result<()> {
        self.line(json!({"op": "sound", "agent": agent_id, "command": command}))
    }
}

#[derive(Default)]
struct Slots {
    /// agent id -> (server notification id, generation)
    map: HashMap<String, (u32, u64)>,
    gen: u64,
}

/// Real notifications via `org.freedesktop.Notifications`.
pub struct DbusBackend {
    icon: String,
    timeout_ms: u32,
    tx: Sender<UiAction>,
    slots: Arc<Mutex<Slots>>,
    conn: Option<zbus::blocking::Connection>,
}

impl DbusBackend {
    pub fn new(cfg: &Config, tx: Sender<UiAction>) -> Self {
        DbusBackend {
            icon: cfg.icon.clone(),
            timeout_ms: cfg.timeout_ms,
            tx,
            slots: Arc::default(),
            conn: None,
        }
    }

    fn connection(&mut self) -> Result<&zbus::blocking::Connection> {
        if self.conn.is_none() {
            self.conn = Some(zbus::blocking::Connection::session()?);
        }
        self.conn.as_ref().ok_or_else(|| anyhow!("no session bus"))
    }
}

impl Backend for DbusBackend {
    fn notify(&mut self, n: &Notice) -> Result<()> {
        let existing = self
            .slots
            .lock()
            .map_err(|_| anyhow!("poisoned"))?
            .map
            .get(&n.agent_id)
            .map(|s| s.0);
        let mut nt = Notification::new();
        nt.appname(APP_NAME)
            .summary(&n.summary)
            .body(&n.body)
            .icon(&self.icon)
            .hint(Hint::Category(n.category.clone()))
            .hint(Hint::DesktopEntry(APP_NAME.to_string()))
            .action("default", "Focus")
            .action("dismiss", "Dismiss");
        match n.urgency {
            Urgency::Critical => {
                nt.urgency(XUrgency::Critical).timeout(Timeout::Never);
            }
            Urgency::Normal => {
                nt.urgency(XUrgency::Normal);
            }
            Urgency::Low => {
                nt.urgency(XUrgency::Low);
            }
        }
        if n.urgency != Urgency::Critical && self.timeout_ms > 0 {
            nt.timeout(Timeout::Milliseconds(self.timeout_ms));
        }
        if let Some(id) = existing {
            nt.id(id);
        }
        let handle = nt.show()?;
        let sid = handle.id();
        let gen = {
            let mut s = self.slots.lock().map_err(|_| anyhow!("poisoned"))?;
            s.gen += 1;
            let g = s.gen;
            s.map.insert(n.agent_id.clone(), (sid, g));
            g
        };

        // One waiter per shown notification. A replaced notification keeps the
        // same server id, so stale waiters are told apart by generation.
        let slots = self.slots.clone();
        let tx = self.tx.clone();
        let agent = n.agent_id.clone();
        std::thread::spawn(move || {
            handle.wait_for_action(|action| {
                let current = {
                    let Ok(mut s) = slots.lock() else { return };
                    let cur = s.map.get(&agent).is_some_and(|&(_, g)| g == gen);
                    if cur && matches!(action, "__closed" | "default" | "focus" | "dismiss") {
                        s.map.remove(&agent);
                    }
                    cur
                };
                if !current {
                    return;
                }
                let ev = match action {
                    "default" | "focus" => UiAction::Focus(agent),
                    "dismiss" => UiAction::Dismiss(agent),
                    "__closed" => UiAction::Closed(agent),
                    _ => return,
                };
                let _ = tx.send(ev);
            });
        });
        Ok(())
    }

    fn close(&mut self, agent_id: &str) -> Result<()> {
        let sid = self
            .slots
            .lock()
            .map_err(|_| anyhow!("poisoned"))?
            .map
            .remove(agent_id)
            .map(|s| s.0);
        let Some(sid) = sid else { return Ok(()) };
        let conn = self.connection()?;
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "CloseNotification",
            &(sid,),
        )?;
        Ok(())
    }

    fn sound(&mut self, agent_id: &str, command: &str) -> Result<()> {
        spawn_sound(agent_id, command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::Transition;

    #[test]
    fn dry_run_lines_are_json() {
        let mut buf = Vec::new();
        {
            let mut b = DryRun::new(&mut buf, &Config::default());
            b.notify(&Notice {
                agent_id: "a".into(),
                name: "api".into(),
                transition: Transition::NeedsInput,
                summary: "⚠ api needs you".into(),
                body: "x".into(),
                urgency: Urgency::Critical,
                category: "x-drove.needs-input".into(),
            })
            .unwrap();
            b.close("a").unwrap();
            b.sound("a", "true").unwrap();
        }
        let lines: Vec<serde_json::Value> = String::from_utf8(buf)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["op"], "notify");
        assert_eq!(lines[0]["urgency"], "critical");
        assert_eq!(lines[0]["transition"], "needs_input");
        assert_eq!(lines[0]["app_name"], "drove");
        assert_eq!(lines[1]["op"], "close");
        assert_eq!(lines[2]["op"], "sound");
    }
}
