//! Stateful part: tracks per-agent state, turns daemon events into backend calls,
//! and handles actions the user triggers on a notification.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use drove_client::{Agent, Event};

use crate::config::Config;
use crate::decide::{decide, Action, Ctx, Notice};

/// Something the user did with a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    /// "Focus" button or click on the notification body.
    Focus(String),
    /// "Dismiss" button.
    Dismiss(String),
    /// The server closed the notification (timeout, swiped away, ...).
    Closed(String),
}

/// Where notifications go: D-Bus, or JSON lines in `--dry-run`.
pub trait Backend {
    fn notify(&mut self, notice: &Notice) -> Result<()>;
    fn close(&mut self, agent_id: &str) -> Result<()>;
    fn sound(&mut self, agent_id: &str, command: &str) -> Result<()>;
}

/// Performs the daemon side of the "Focus" action.
pub trait Focuser {
    fn focus(&mut self, agent_id: &str) -> Result<()>;
}

/// Focuses over a fresh connection each time (survives daemon restarts).
pub struct SocketFocuser {
    pub socket: Option<PathBuf>,
}

impl Focuser for SocketFocuser {
    fn focus(&mut self, agent_id: &str) -> Result<()> {
        let mut c = match &self.socket {
            Some(p) => drove_client::Client::connect_to(p)?,
            None => drove_client::Client::connect()?,
        };
        c.focus(agent_id)?;
        Ok(())
    }
}

/// Per-agent bookkeeping around [`decide`].
pub struct Engine {
    cfg: Config,
    quiet_file: Option<PathBuf>,
    prev: HashMap<String, Agent>,
    last_notified: HashMap<String, Instant>,
    /// Agents with a notification currently shown.
    open: HashSet<String>,
    /// Agents whose exit we asked for.
    closing: HashSet<String>,
}

impl Engine {
    pub fn new(cfg: Config, quiet_file: Option<PathBuf>) -> Self {
        Engine {
            cfg,
            quiet_file,
            prev: HashMap::new(),
            last_notified: HashMap::new(),
            open: HashSet::new(),
            closing: HashSet::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Record that the notifier itself asked for this agent to be closed, so
    /// its coming exit is not announced.
    pub fn expect_exit(&mut self, agent_id: &str) {
        self.closing.insert(agent_id.to_string());
    }

    pub fn is_open(&self, agent_id: &str) -> bool {
        self.open.contains(agent_id)
    }

    pub fn forget_open(&mut self, agent_id: &str) {
        self.open.remove(agent_id);
    }

    fn quiet(&self) -> bool {
        self.cfg.quiet || self.quiet_file.as_ref().is_some_and(|p| p.exists())
    }

    pub fn handle(&mut self, ev: Event, now: Instant) -> Vec<Action> {
        match ev {
            Event::Snapshot { agents } => {
                // Replace state; never notify. Only drop notifications that no
                // longer apply.
                let mut out = Vec::new();
                let old: Vec<String> = self.open.iter().cloned().collect();
                for id in old {
                    let still = agents.iter().find(|a| a.id == id).is_some_and(|a| {
                        use drove_client::Status::*;
                        match a.status {
                            NeedsInput => true,
                            Idle => a.attention,
                            Exited => true,
                            Working | Starting => false,
                        }
                    });
                    if !still {
                        self.open.remove(&id);
                        out.push(Action::Close { agent_id: id });
                    }
                }
                self.prev = agents.into_iter().map(|a| (a.id.clone(), a)).collect();
                self.last_notified
                    .retain(|id, _| self.prev.contains_key(id));
                self.closing.retain(|id| self.prev.contains_key(id));
                out
            }
            Event::Removed { id } => {
                self.prev.remove(&id);
                self.last_notified.remove(&id);
                self.closing.remove(&id);
                if self.open.remove(&id) {
                    vec![Action::Close { agent_id: id }]
                } else {
                    vec![]
                }
            }
            Event::Agent { agent } => {
                let agent = *agent;
                let ctx = Ctx {
                    cfg: &self.cfg,
                    quiet: self.quiet(),
                    now,
                    last_notified: self.last_notified.get(&agent.id).copied(),
                    expected_exit: self.closing.contains(&agent.id),
                };
                let actions = decide(self.prev.get(&agent.id), &agent, &ctx);
                let mut out = Vec::new();
                for a in actions {
                    match &a {
                        Action::Notify(n) => {
                            self.last_notified.insert(n.agent_id.clone(), now);
                            self.open.insert(n.agent_id.clone());
                            out.push(a);
                        }
                        Action::Close { agent_id } => {
                            // Only if something is actually shown.
                            if self.open.remove(agent_id) {
                                out.push(a);
                            }
                        }
                        Action::Sound { .. } => out.push(a),
                    }
                }
                self.prev.insert(agent.id.clone(), agent);
                out
            }
        }
    }
}

pub struct App<B: Backend, F: Focuser> {
    pub engine: Engine,
    pub backend: B,
    pub focuser: F,
}

impl<B: Backend, F: Focuser> App<B, F> {
    pub fn new(engine: Engine, backend: B, focuser: F) -> Self {
        App {
            engine,
            backend,
            focuser,
        }
    }

    pub fn on_event(&mut self, ev: Event) {
        self.on_event_at(ev, Instant::now());
    }

    pub fn on_event_at(&mut self, ev: Event, now: Instant) {
        for action in self.engine.handle(ev, now) {
            let r = match action {
                Action::Notify(n) => self.backend.notify(&n),
                Action::Close { agent_id } => self.backend.close(&agent_id),
                Action::Sound {
                    agent_id, command, ..
                } => self.backend.sound(&agent_id, &command),
            };
            if let Err(e) = r {
                eprintln!("drove-notify: {e:#}");
            }
        }
    }

    /// Handle a user action on a notification.
    pub fn on_ui(&mut self, action: UiAction) {
        match action {
            UiAction::Focus(id) => {
                if let Err(e) = self.focuser.focus(&id) {
                    eprintln!("drove-notify: focus {id}: {e:#}");
                }
                if self.engine.is_open(&id) {
                    self.engine.forget_open(&id);
                    let _ = self.backend.close(&id);
                }
            }
            UiAction::Dismiss(id) => {
                if self.engine.is_open(&id) {
                    self.engine.forget_open(&id);
                    let _ = self.backend.close(&id);
                }
            }
            UiAction::Closed(id) => self.engine.forget_open(&id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drove_client::Status;
    use serde_json::json;
    use std::time::Duration;

    #[derive(Default)]
    struct Rec {
        log: Vec<String>,
    }
    impl Backend for Rec {
        fn notify(&mut self, n: &Notice) -> Result<()> {
            self.log
                .push(format!("notify {} {}", n.agent_id, n.summary));
            Ok(())
        }
        fn close(&mut self, id: &str) -> Result<()> {
            self.log.push(format!("close {id}"));
            Ok(())
        }
        fn sound(&mut self, id: &str, c: &str) -> Result<()> {
            self.log.push(format!("sound {id} {c}"));
            Ok(())
        }
    }
    #[derive(Default)]
    struct FocusRec(Vec<String>);
    impl Focuser for FocusRec {
        fn focus(&mut self, id: &str) -> Result<()> {
            self.0.push(id.to_string());
            Ok(())
        }
    }

    fn ag(id: &str, status: Status, attention: bool) -> Agent {
        serde_json::from_value(json!({
            "id": id, "name": id, "kind": "claude", "status": status,
            "attention": attention, "detail": "d",
        }))
        .unwrap()
    }
    fn snap(v: Vec<Agent>) -> Event {
        Event::Snapshot { agents: v }
    }
    fn upd(a: Agent) -> Event {
        Event::Agent { agent: Box::new(a) }
    }
    fn app(cfg: Config) -> App<Rec, FocusRec> {
        App::new(Engine::new(cfg, None), Rec::default(), FocusRec::default())
    }

    #[test]
    fn snapshot_never_notifies() {
        let mut a = app(Config::default());
        a.on_event(snap(vec![
            ag("a", Status::NeedsInput, true),
            ag("b", Status::Idle, true),
            ag("c", Status::Exited, false),
        ]));
        assert!(a.backend.log.is_empty());
        // re-snapshot with changed states: still silent
        a.on_event(snap(vec![
            ag("a", Status::Working, false),
            ag("b", Status::NeedsInput, true),
        ]));
        assert!(a.backend.log.is_empty());
        // ... but diffs against the re-snapshot work
        a.on_event(upd(ag("a", Status::NeedsInput, true)));
        assert_eq!(a.backend.log, ["notify a ⚠ a needs you"]);
    }

    #[test]
    fn replaced_not_stacked_and_closed_on_working() {
        let mut a = app(Config {
            rate_limit_secs: 0.0,
            ..Config::default()
        });
        a.on_event(snap(vec![ag("a", Status::Working, false)]));
        a.on_event(upd(ag("a", Status::NeedsInput, true)));
        a.on_event(upd(ag("a", Status::Working, false)));
        a.on_event(upd(ag("a", Status::Working, false))); // nothing open: no second close
        a.on_event(upd(ag("a", Status::Idle, true)));
        a.on_event(upd(ag("a", Status::Idle, false))); // focused elsewhere
        assert_eq!(
            a.backend.log,
            [
                "notify a ⚠ a needs you",
                "close a",
                "notify a ✓ a finished",
                "close a"
            ]
        );
    }

    #[test]
    fn rate_limit_per_agent() {
        let mut a = app(Config::default());
        let t0 = Instant::now();
        a.on_event_at(
            snap(vec![
                ag("a", Status::Working, false),
                ag("b", Status::Working, false),
            ]),
            t0,
        );
        a.on_event_at(upd(ag("a", Status::NeedsInput, true)), t0);
        a.on_event_at(
            upd(ag("a", Status::Working, false)),
            t0 + Duration::from_secs(1),
        );
        a.on_event_at(
            upd(ag("a", Status::NeedsInput, true)),
            t0 + Duration::from_secs(2),
        ); // limited
        a.on_event_at(
            upd(ag("b", Status::NeedsInput, true)),
            t0 + Duration::from_secs(2),
        ); // other agent ok
        a.on_event_at(
            upd(ag("a", Status::Working, false)),
            t0 + Duration::from_secs(3),
        );
        a.on_event_at(
            upd(ag("a", Status::NeedsInput, true)),
            t0 + Duration::from_secs(11),
        );
        let n = a
            .backend
            .log
            .iter()
            .filter(|l| l.starts_with("notify"))
            .count();
        assert_eq!(n, 3, "{:?}", a.backend.log);
    }

    #[test]
    fn quiet_file_toggles_at_runtime() {
        let dir = std::env::temp_dir().join(format!("drove-notify-q-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let qf = dir.join("quiet");
        let _ = std::fs::remove_file(&qf);
        let mut a = App::new(
            Engine::new(
                Config {
                    rate_limit_secs: 0.0,
                    ..Config::default()
                },
                Some(qf.clone()),
            ),
            Rec::default(),
            FocusRec::default(),
        );
        a.on_event(snap(vec![ag("a", Status::Working, false)]));
        std::fs::write(&qf, "").unwrap();
        a.on_event(upd(ag("a", Status::NeedsInput, true)));
        assert!(a.backend.log.is_empty());
        std::fs::remove_file(&qf).unwrap();
        a.on_event(upd(ag("a", Status::Working, false)));
        a.on_event(upd(ag("a", Status::NeedsInput, true)));
        assert_eq!(a.backend.log.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn expected_exit_is_silent() {
        let mut a = app(Config::default());
        a.on_event(snap(vec![
            ag("a", Status::Idle, false),
            ag("b", Status::Idle, false),
        ]));
        a.engine.expect_exit("a");
        a.on_event(upd(ag("a", Status::Exited, false)));
        assert!(a.backend.log.is_empty());
        a.on_event(upd(ag("b", Status::Exited, false)));
        assert_eq!(a.backend.log, ["notify b × b exited"]);
    }

    #[test]
    fn removed_closes_open_notification() {
        let mut a = app(Config::default());
        a.on_event(snap(vec![ag("a", Status::Working, false)]));
        a.on_event(upd(ag("a", Status::NeedsInput, true)));
        a.on_event(Event::Removed { id: "a".into() });
        assert_eq!(a.backend.log, ["notify a ⚠ a needs you", "close a"]);
    }

    #[test]
    fn snapshot_closes_stale_notification() {
        let mut a = app(Config::default());
        a.on_event(snap(vec![ag("a", Status::Working, false)]));
        a.on_event(upd(ag("a", Status::NeedsInput, true)));
        a.on_event(snap(vec![ag("a", Status::Working, false)]));
        assert_eq!(a.backend.log, ["notify a ⚠ a needs you", "close a"]);
    }

    #[test]
    fn focus_action_focuses_and_closes() {
        let mut a = app(Config::default());
        a.on_event(snap(vec![ag("a", Status::Working, false)]));
        a.on_event(upd(ag("a", Status::NeedsInput, true)));
        a.on_ui(UiAction::Focus("a".into()));
        assert_eq!(a.focuser.0, ["a"]);
        assert_eq!(a.backend.log.last().unwrap(), "close a");
    }

    #[test]
    fn dismiss_closes_without_focus() {
        let mut a = app(Config::default());
        a.on_event(snap(vec![ag("a", Status::Working, false)]));
        a.on_event(upd(ag("a", Status::NeedsInput, true)));
        a.on_ui(UiAction::Dismiss("a".into()));
        assert!(a.focuser.0.is_empty());
        assert_eq!(a.backend.log.last().unwrap(), "close a");
    }
}
