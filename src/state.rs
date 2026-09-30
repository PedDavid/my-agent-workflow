//! The agent store: applies hook signals and WM events, reports what changed.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::adapters::{Hook, Signal, Source};
use crate::model::{Agent, AgentKind, Status, WindowRef};
use crate::wm::{WmEvent, id_from_class};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Updated(String),
    Removed(String),
}

#[derive(Debug, Default)]
pub struct Store {
    pub agents: BTreeMap<String, Agent>,
    /// Address of the focused window, if any.
    pub focused: Option<String>,
    pub kiro_stale_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    version: u32,
    agents: Vec<Agent>,
}

impl Store {
    pub fn new(kiro_stale_ms: u64) -> Self {
        Store {
            kiro_stale_ms,
            ..Default::default()
        }
    }

    pub fn snapshot(&self) -> Vec<Agent> {
        let mut v: Vec<Agent> = self.agents.values().cloned().collect();
        v.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        v
    }

    pub fn get(&self, id: &str) -> Option<&Agent> {
        self.agents.get(id)
    }

    pub fn insert(&mut self, agent: Agent) -> Change {
        let id = agent.id.clone();
        self.agents.insert(id.clone(), agent);
        Change::Updated(id)
    }

    pub fn by_address(&self, address: &str) -> Option<String> {
        self.agents
            .values()
            .find(|a| a.window.as_ref().is_some_and(|w| w.address == address))
            .map(|a| a.id.clone())
    }

    fn is_focused(&self, a: &Agent) -> bool {
        match (&a.window, &self.focused) {
            (Some(w), Some(f)) => &w.address == f,
            _ => false,
        }
    }

    /// Resolve an id, a name, or a unique id prefix.
    pub fn resolve(&self, key: &str) -> Result<String> {
        if self.agents.contains_key(key) {
            return Ok(key.to_string());
        }
        let by_name: Vec<&Agent> = self.agents.values().filter(|a| a.name == key).collect();
        match by_name.len() {
            1 => return Ok(by_name[0].id.clone()),
            n if n > 1 => {
                // Prefer a live agent over exited ones with the same name.
                let live: Vec<&&Agent> = by_name
                    .iter()
                    .filter(|a| a.status != Status::Exited)
                    .collect();
                if live.len() == 1 {
                    return Ok(live[0].id.clone());
                }
                bail!("name {key:?} is ambiguous; use the id")
            }
            _ => {}
        }
        let pre: Vec<&String> = self.agents.keys().filter(|k| k.starts_with(key)).collect();
        match pre.len() {
            1 if !key.is_empty() => Ok(pre[0].clone()),
            0 | 1 => bail!("no agent {key:?}"),
            _ => bail!("id prefix {key:?} is ambiguous"),
        }
    }

    /// `base`, or `base-2`, `base-3`… if taken by a live agent.
    pub fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| {
            self.agents
                .values()
                .any(|a| a.name == n && a.status != Status::Exited)
        };
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|i| format!("{base}-{i}"))
            .find(|n| !taken(n))
            .unwrap()
    }

    pub fn apply_hook(&mut self, id: &str, source: Source, hook: &Hook, now: u64) -> Vec<Change> {
        let focused = self
            .agents
            .get(id)
            .map(|a| self.is_focused(a))
            .unwrap_or(false);
        let is_kiro = source == Source::Kiro;
        let Some(a) = self.agents.get_mut(id) else {
            return vec![];
        };
        let before = a.clone();
        a.last_hook_at = Some(now);
        if a.kind == AgentKind::Generic {
            a.kind = source.kind();
        }
        if let Some(sid) = &hook.session_id {
            a.session_id = Some(sid.clone());
        }
        match &hook.signal {
            Signal::SessionStarted => {
                if matches!(a.status, Status::Starting | Status::Exited | Status::Idle) {
                    a.status = Status::Idle;
                    a.detail.clear();
                }
            }
            Signal::PromptSubmitted { prompt } => {
                a.status = Status::Working;
                a.detail = prompt.clone();
                a.attention = false;
                a.maybe = false;
                a.pending_tool_since = None;
            }
            Signal::ToolStart { name } => {
                a.status = Status::Working;
                a.detail = name.clone();
                a.maybe = false;
                if is_kiro {
                    a.pending_tool_since = Some(now);
                }
            }
            Signal::ToolEnd { .. } => {
                if a.status == Status::NeedsInput {
                    a.attention = false;
                }
                a.status = Status::Working;
                a.maybe = false;
                a.pending_tool_since = None;
            }
            Signal::NeedsInput { msg } => {
                a.status = Status::NeedsInput;
                a.detail = msg.clone();
                a.attention = true;
                a.maybe = false;
            }
            Signal::TurnDone { last_message } => {
                a.status = Status::Idle;
                a.detail = last_message.clone().unwrap_or_default();
                a.attention = !focused;
                a.maybe = false;
                a.pending_tool_since = None;
            }
            Signal::Idle => {
                a.status = Status::Idle;
                a.maybe = false;
                a.pending_tool_since = None;
            }
            Signal::SessionEnded => {
                a.status = Status::Exited;
                a.detail = "session ended".into();
                a.attention = false;
                a.maybe = false;
                a.pending_tool_since = None;
            }
            Signal::Ignore => {}
        }
        if changed(&before, a) {
            a.updated_at = now;
            vec![Change::Updated(id.to_string())]
        } else {
            vec![]
        }
    }

    pub fn apply_wm(&mut self, ev: &WmEvent, now: u64) -> Vec<Change> {
        match ev {
            WmEvent::OpenWindow {
                address,
                workspace,
                class,
                title,
            } => {
                let Some(id) = id_from_class(class) else {
                    return vec![];
                };
                let win = WindowRef {
                    address: address.clone(),
                    workspace: workspace.clone(),
                    title: title.clone(),
                    class: class.clone(),
                };
                if let Some(a) = self.agents.get_mut(id) {
                    a.window = Some(win);
                    if a.status == Status::Exited {
                        a.status = Status::Starting;
                    }
                    if a.kind == AgentKind::Generic && a.status == Status::Starting {
                        a.status = Status::Idle;
                    }
                    a.updated_at = now;
                } else {
                    // A drove window we don't know (state lost): adopt it.
                    let mut a = Agent::new(id, title, AgentKind::Generic, "", "", now);
                    a.name = self.unique_name(if title.is_empty() { id } else { title });
                    a.status = Status::Idle;
                    a.adopted = true;
                    a.window = Some(win);
                    self.agents.insert(id.to_string(), a);
                }
                vec![Change::Updated(id.to_string())]
            }
            WmEvent::CloseWindow { address } => {
                if self.focused.as_deref() == Some(address) {
                    self.focused = None;
                }
                let Some(id) = self.by_address(address) else {
                    return vec![];
                };
                let a = self.agents.get_mut(&id).unwrap();
                a.window = None;
                a.status = Status::Exited;
                a.attention = false;
                a.maybe = false;
                a.pending_tool_since = None;
                a.updated_at = now;
                vec![Change::Updated(id)]
            }
            WmEvent::ActiveWindow { address } => {
                self.focused = address.clone();
                let Some(id) = address.as_deref().and_then(|a| self.by_address(a)) else {
                    return vec![];
                };
                let a = self.agents.get_mut(&id).unwrap();
                if a.attention {
                    a.attention = false;
                    a.updated_at = now;
                    vec![Change::Updated(id)]
                } else {
                    vec![]
                }
            }
            WmEvent::Title { address, title } => self.with_window(address, now, |w| {
                if &w.title == title {
                    return false;
                }
                w.title = title.clone();
                true
            }),
            WmEvent::MoveWindow { address, workspace } => self.with_window(address, now, |w| {
                if &w.workspace == workspace {
                    return false;
                }
                w.workspace = workspace.clone();
                true
            }),
            WmEvent::Urgent { address } => {
                if self.focused.as_deref() == Some(address) {
                    return vec![];
                }
                let Some(id) = self.by_address(address) else {
                    return vec![];
                };
                let a = self.agents.get_mut(&id).unwrap();
                if a.attention || a.status == Status::Exited {
                    return vec![];
                }
                a.attention = true;
                a.updated_at = now;
                vec![Change::Updated(id)]
            }
        }
    }

    fn with_window(
        &mut self,
        address: &str,
        now: u64,
        f: impl FnOnce(&mut WindowRef) -> bool,
    ) -> Vec<Change> {
        let Some(id) = self.by_address(address) else {
            return vec![];
        };
        let a = self.agents.get_mut(&id).unwrap();
        if f(a.window.as_mut().unwrap()) {
            a.updated_at = now;
            vec![Change::Updated(id)]
        } else {
            vec![]
        }
    }

    /// Periodic housekeeping: the kiro stale-tool heuristic.
    pub fn tick(&mut self, now: u64) -> Vec<Change> {
        let mut out = vec![];
        let stale = self.kiro_stale_ms;
        for a in self.agents.values_mut() {
            if a.kind != AgentKind::Kiro || a.status != Status::Working || a.maybe {
                continue;
            }
            if let Some(since) = a.pending_tool_since
                && now.saturating_sub(since) >= stale
            {
                a.status = Status::NeedsInput;
                a.maybe = true;
                a.attention = true;
                a.detail = format!("waiting? {}", a.detail);
                a.updated_at = now;
                out.push(Change::Updated(a.id.clone()));
            }
        }
        out
    }

    /// Which agent `drove next` should jump to: NeedsInput, then idle with
    /// attention, then anything with attention. The focused agent is skipped
    /// unless it is the only candidate.
    pub fn next(&self) -> Option<String> {
        let live = || self.agents.values().filter(|a| a.status != Status::Exited);
        let groups: [Vec<&Agent>; 3] = [
            live().filter(|a| a.status == Status::NeedsInput).collect(),
            live()
                .filter(|a| a.attention && a.status == Status::Idle)
                .collect(),
            live().filter(|a| a.attention).collect(),
        ];
        let mut fallback = None;
        for mut g in groups {
            g.sort_by_key(|a| (a.updated_at, a.created_at));
            for a in g {
                if self.is_focused(a) {
                    fallback.get_or_insert_with(|| a.id.clone());
                } else {
                    return Some(a.id.clone());
                }
            }
        }
        fallback
    }

    pub fn forget(&mut self, id: &str) -> Vec<Change> {
        match self.agents.remove(id) {
            Some(_) => vec![Change::Removed(id.to_string())],
            None => vec![],
        }
    }

    pub fn forget_exited(&mut self) -> Vec<Change> {
        let ids: Vec<String> = self
            .agents
            .values()
            .filter(|a| a.status == Status::Exited)
            .map(|a| a.id.clone())
            .collect();
        ids.iter().flat_map(|id| self.forget(id)).collect()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&StateFile {
            version: 1,
            agents: self.snapshot(),
        })
        .unwrap()
    }

    pub fn load_json(&mut self, s: &str) -> Result<()> {
        let f: StateFile = serde_json::from_str(s)?;
        self.agents = f.agents.into_iter().map(|a| (a.id.clone(), a)).collect();
        Ok(())
    }

    /// Atomic write (tmp + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn load(&mut self, path: &Path) -> Result<()> {
        match std::fs::read_to_string(path) {
            Ok(s) => self.load_json(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

fn changed(a: &Agent, b: &Agent) -> bool {
    let mut b = b.clone();
    b.last_hook_at = a.last_hook_at;
    b.updated_at = a.updated_at;
    a != &b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(signal: Signal) -> Hook {
        Hook {
            signal,
            session_id: None,
            cwd: None,
        }
    }

    fn store_with(id: &str, kind: AgentKind) -> Store {
        let mut s = Store::new(4000);
        s.insert(Agent::new(id, "n", kind, "p", "/w", 1));
        s
    }

    fn open(s: &mut Store, id: &str, addr: &str) {
        s.apply_wm(
            &WmEvent::OpenWindow {
                address: addr.into(),
                workspace: "3".into(),
                class: format!("drove-{id}"),
                title: "t".into(),
            },
            2,
        );
    }

    #[test]
    fn lifecycle() {
        let mut s = store_with("aaaaaa", AgentKind::Claude);
        open(&mut s, "aaaaaa", "0x1");
        assert_eq!(
            s.get("aaaaaa").unwrap().window.as_ref().unwrap().address,
            "0x1"
        );
        assert_eq!(s.get("aaaaaa").unwrap().status, Status::Starting);

        let mut h = hook(Signal::SessionStarted);
        h.session_id = Some("sess".into());
        assert_eq!(s.apply_hook("aaaaaa", Source::Claude, &h, 3).len(), 1);
        let a = s.get("aaaaaa").unwrap();
        assert_eq!(a.status, Status::Idle);
        assert_eq!(a.session_id.as_deref(), Some("sess"));

        s.apply_hook(
            "aaaaaa",
            Source::Claude,
            &hook(Signal::PromptSubmitted {
                prompt: "hi".into(),
            }),
            4,
        );
        assert_eq!(s.get("aaaaaa").unwrap().status, Status::Working);
        assert_eq!(s.get("aaaaaa").unwrap().detail, "hi");

        s.apply_hook(
            "aaaaaa",
            Source::Claude,
            &hook(Signal::ToolStart {
                name: "Bash".into(),
            }),
            5,
        );
        assert_eq!(s.get("aaaaaa").unwrap().detail, "Bash");

        s.apply_hook(
            "aaaaaa",
            Source::Claude,
            &hook(Signal::NeedsInput { msg: "perm".into() }),
            6,
        );
        let a = s.get("aaaaaa").unwrap();
        assert_eq!(a.status, Status::NeedsInput);
        assert!(a.attention);

        s.apply_hook(
            "aaaaaa",
            Source::Claude,
            &hook(Signal::ToolEnd {
                name: "Bash".into(),
            }),
            7,
        );
        let a = s.get("aaaaaa").unwrap();
        assert_eq!(a.status, Status::Working);
        assert!(!a.attention);

        s.apply_hook(
            "aaaaaa",
            Source::Claude,
            &hook(Signal::TurnDone {
                last_message: Some("done".into()),
            }),
            8,
        );
        let a = s.get("aaaaaa").unwrap();
        assert_eq!(a.status, Status::Idle);
        assert!(a.attention);
        assert_eq!(a.updated_at, 8);

        // focusing the window clears attention
        let ch = s.apply_wm(
            &WmEvent::ActiveWindow {
                address: Some("0x1".into()),
            },
            9,
        );
        assert_eq!(ch, vec![Change::Updated("aaaaaa".into())]);
        assert!(!s.get("aaaaaa").unwrap().attention);

        // TurnDone while focused: no attention
        s.apply_hook(
            "aaaaaa",
            Source::Claude,
            &hook(Signal::TurnDone { last_message: None }),
            10,
        );
        assert!(!s.get("aaaaaa").unwrap().attention);

        s.apply_wm(
            &WmEvent::CloseWindow {
                address: "0x1".into(),
            },
            11,
        );
        let a = s.get("aaaaaa").unwrap();
        assert_eq!(a.status, Status::Exited);
        assert!(a.window.is_none());
        assert_eq!(s.focused, None);
    }

    #[test]
    fn ignore_does_not_report_change() {
        let mut s = store_with("aaaaaa", AgentKind::Claude);
        assert!(
            s.apply_hook("aaaaaa", Source::Claude, &hook(Signal::Ignore), 5)
                .is_empty()
        );
        assert_eq!(s.get("aaaaaa").unwrap().last_hook_at, Some(5));
        assert!(
            s.apply_hook("nope00", Source::Claude, &hook(Signal::Idle), 5)
                .is_empty()
        );
    }

    #[test]
    fn session_end_and_generic_kind_upgrade() {
        let mut s = store_with("aaaaaa", AgentKind::Generic);
        s.apply_hook("aaaaaa", Source::Codex, &hook(Signal::SessionEnded), 3);
        let a = s.get("aaaaaa").unwrap();
        assert_eq!(a.status, Status::Exited);
        assert_eq!(a.kind, AgentKind::Codex);
    }

    #[test]
    fn generic_becomes_idle_on_open() {
        let mut s = store_with("aaaaaa", AgentKind::Generic);
        open(&mut s, "aaaaaa", "0x1");
        assert_eq!(s.get("aaaaaa").unwrap().status, Status::Idle);
    }

    #[test]
    fn unknown_drove_window_is_adopted() {
        let mut s = Store::new(1);
        s.apply_wm(
            &WmEvent::OpenWindow {
                address: "0x9".into(),
                workspace: "1".into(),
                class: "drove-zzzzzz".into(),
                title: "claude".into(),
            },
            1,
        );
        let a = s.get("zzzzzz").unwrap();
        assert!(a.adopted);
        assert_eq!(a.name, "claude");
        // non-drove windows are ignored
        assert!(
            s.apply_wm(
                &WmEvent::OpenWindow {
                    address: "0x8".into(),
                    workspace: "1".into(),
                    class: "firefox".into(),
                    title: "x".into(),
                },
                1
            )
            .is_empty()
        );
    }

    #[test]
    fn title_move_urgent() {
        let mut s = store_with("aaaaaa", AgentKind::Claude);
        open(&mut s, "aaaaaa", "0x1");
        s.apply_wm(
            &WmEvent::Title {
                address: "0x1".into(),
                title: "new".into(),
            },
            3,
        );
        s.apply_wm(
            &WmEvent::MoveWindow {
                address: "0x1".into(),
                workspace: "special:agents".into(),
            },
            3,
        );
        let w = s.get("aaaaaa").unwrap().window.clone().unwrap();
        assert_eq!(w.title, "new");
        assert_eq!(w.workspace, "special:agents");
        assert_eq!(
            s.apply_wm(
                &WmEvent::Urgent {
                    address: "0x1".into()
                },
                4
            )
            .len(),
            1
        );
        assert!(s.get("aaaaaa").unwrap().attention);
    }

    #[test]
    fn kiro_stale_tool() {
        let mut s = store_with("kkkkkk", AgentKind::Kiro);
        s.apply_hook(
            "kkkkkk",
            Source::Kiro,
            &hook(Signal::ToolStart {
                name: "execute_bash".into(),
            }),
            1000,
        );
        assert!(s.tick(3000).is_empty());
        assert_eq!(s.tick(5000).len(), 1);
        let a = s.get("kkkkkk").unwrap();
        assert_eq!(a.status, Status::NeedsInput);
        assert!(a.maybe && a.attention);
        assert!(s.tick(6000).is_empty(), "fires once");
        s.apply_hook(
            "kkkkkk",
            Source::Kiro,
            &hook(Signal::ToolEnd {
                name: "execute_bash".into(),
            }),
            7000,
        );
        let a = s.get("kkkkkk").unwrap();
        assert_eq!(a.status, Status::Working);
        assert!(!a.maybe && !a.attention);
        // claude tools never go stale
        let mut c = store_with("cccccc", AgentKind::Claude);
        c.apply_hook(
            "cccccc",
            Source::Claude,
            &hook(Signal::ToolStart { name: "x".into() }),
            0,
        );
        assert!(c.tick(100_000).is_empty());
    }

    #[test]
    fn next_priority() {
        let mut s = Store::new(1);
        for (i, id) in ["aaaaaa", "bbbbbb", "cccccc", "dddddd"].iter().enumerate() {
            s.insert(Agent::new(id, id, AgentKind::Claude, "", "", i as u64));
        }
        assert_eq!(s.next(), None);
        let a = s.agents.get_mut("aaaaaa").unwrap();
        a.status = Status::Working;
        a.attention = true;
        a.updated_at = 1;
        let b = s.agents.get_mut("bbbbbb").unwrap();
        b.status = Status::Idle;
        b.attention = true;
        b.updated_at = 5;
        assert_eq!(s.next().as_deref(), Some("bbbbbb"));
        let c = s.agents.get_mut("cccccc").unwrap();
        c.status = Status::NeedsInput;
        c.updated_at = 9;
        c.window = Some(WindowRef {
            address: "0xc".into(),
            ..Default::default()
        });
        assert_eq!(s.next().as_deref(), Some("cccccc"));
        // focused agent is skipped when others are waiting
        s.focused = Some("0xc".into());
        assert_eq!(s.next().as_deref(), Some("bbbbbb"));
        s.agents.get_mut("bbbbbb").unwrap().attention = false;
        s.agents.get_mut("aaaaaa").unwrap().attention = false;
        assert_eq!(s.next().as_deref(), Some("cccccc"));
    }

    #[test]
    fn resolve_and_names() {
        let mut s = Store::new(1);
        s.insert(Agent::new("abcdef", "api", AgentKind::Claude, "", "", 0));
        s.insert(Agent::new("abzzzz", "web", AgentKind::Claude, "", "", 0));
        assert_eq!(s.resolve("abcdef").unwrap(), "abcdef");
        assert_eq!(s.resolve("web").unwrap(), "abzzzz");
        assert_eq!(s.resolve("abc").unwrap(), "abcdef");
        assert!(s.resolve("ab").is_err());
        assert!(s.resolve("nope").is_err());
        assert!(s.resolve("").is_err());
        assert_eq!(s.unique_name("api"), "api-2");
        assert_eq!(s.unique_name("x"), "x");
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/state.json");
        let mut s = store_with("aaaaaa", AgentKind::Kiro);
        open(&mut s, "aaaaaa", "0x1");
        s.save(&path).unwrap();
        let mut t = Store::new(1);
        t.load(&path).unwrap();
        assert_eq!(t.snapshot(), s.snapshot());
        let mut u = Store::new(1);
        u.load(&dir.path().join("missing.json")).unwrap();
        assert!(u.agents.is_empty());
    }

    #[test]
    fn forget() {
        let mut s = store_with("aaaaaa", AgentKind::Claude);
        s.insert(Agent::new("bbbbbb", "b", AgentKind::Claude, "", "", 0));
        s.agents.get_mut("bbbbbb").unwrap().status = Status::Exited;
        assert_eq!(s.forget_exited(), vec![Change::Removed("bbbbbb".into())]);
        assert_eq!(s.forget("aaaaaa").len(), 1);
        assert!(s.forget("aaaaaa").is_empty());
    }
}
