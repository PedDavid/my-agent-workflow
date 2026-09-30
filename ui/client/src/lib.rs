//! Client for the drove daemon protocol (see `docs/protocol.md`).
//!
//! Blocking and dependency-light on purpose: TUIs can use it from a thread,
//! async programs from `spawn_blocking` / a dedicated thread feeding a channel.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Starting,
    Idle,
    Working,
    NeedsInput,
    Exited,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Starting => "starting",
            Status::Idle => "idle",
            Status::Working => "working",
            Status::NeedsInput => "needs_input",
            Status::Exited => "exited",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WindowRef {
    pub address: String,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub class: String,
}

/// An agent as reported by the daemon. Unknown fields are ignored so older
/// UIs keep working against newer daemons.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// `claude` | `codex` | `kiro` | `generic` (kept as a string for forward compat).
    pub kind: String,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub cwd: String,
    pub status: Status,
    #[serde(default)]
    pub attention: bool,
    #[serde(default)]
    pub detail: String,
    /// Status is a heuristic guess.
    #[serde(default)]
    pub maybe: bool,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub window: Option<WindowRef>,
    #[serde(default)]
    pub term: Option<Value>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub adopted: bool,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub last_hook_at: Option<u64>,
}

impl Agent {
    /// Ordering used by `drove next`: needs_input, attention+idle, working,
    /// idle, starting, exited; newest first within a group.
    pub fn priority(&self) -> (u8, std::cmp::Reverse<u64>) {
        let r = match (self.status, self.attention) {
            (Status::NeedsInput, _) => 0,
            (Status::Idle, true) => 1,
            (Status::Working, _) => 2,
            (Status::Idle, false) => 3,
            (Status::Starting, _) => 4,
            (Status::Exited, _) => 5,
        };
        (r, std::cmp::Reverse(self.updated_at))
    }
}

/// Sort agents in `drove next` order.
pub fn sort_agents(agents: &mut [Agent]) {
    agents.sort_by_key(|a| a.priority());
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Snapshot { agents: Vec<Agent> },
    Agent { agent: Box<Agent> },
    Removed { id: String },
}

/// Apply an event to a local list (replace on snapshot, upsert, remove).
pub fn apply(agents: &mut Vec<Agent>, ev: Event) {
    match ev {
        Event::Snapshot { agents: a } => *agents = a,
        Event::Agent { agent } => match agents.iter_mut().find(|x| x.id == agent.id) {
            Some(x) => *x = *agent,
            None => agents.push(*agent),
        },
        Event::Removed { id } => agents.retain(|x| x.id != id),
    }
}

/// `$DROVE_SOCKET`, else `$XDG_RUNTIME_DIR/drove/drove.sock`, else `/tmp/drove-$UID/drove.sock`.
pub fn socket_path() -> PathBuf {
    let env = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
    if let Some(s) = env("DROVE_SOCKET") {
        return s.into();
    }
    match env("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("drove").join("drove.sock"),
        None => {
            // Avoid a libc dependency: read our uid from /proc.
            let uid = std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("Uid:"))
                        .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
                })
                .unwrap_or_else(|| "0".into());
            PathBuf::from(format!("/tmp/drove-{uid}/drove.sock"))
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    /// The daemon answered `ok: false`.
    Daemon(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "drove socket: {e}"),
            Error::Json(e) => write!(f, "drove protocol: {e}"),
            Error::Daemon(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A request/response connection. Not for `subscribe`, see [`Subscription`].
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Client {
    pub fn connect() -> Result<Self> {
        Self::connect_to(&socket_path())
    }

    pub fn connect_to(path: &std::path::Path) -> Result<Self> {
        let s = UnixStream::connect(path)?;
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok(Client {
            reader: BufReader::new(s.try_clone()?),
            writer: s,
            next_id: 1,
        })
    }

    /// Send any method; returns `result` or the daemon's error.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line =
            serde_json::to_string(&json!({"id": id, "method": method, "params": params}))?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        let mut resp = String::new();
        if self.reader.read_line(&mut resp)? == 0 {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "daemon closed connection",
            )));
        }
        let v: Value = serde_json::from_str(&resp)?;
        if v["ok"].as_bool() == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(Error::Daemon(
                v["error"].as_str().unwrap_or("unknown error").to_string(),
            ))
        }
    }

    pub fn list(&mut self) -> Result<Vec<Agent>> {
        Ok(serde_json::from_value(self.call("list", Value::Null)?)?)
    }

    pub fn focus(&mut self, id: &str) -> Result<()> {
        self.call("focus", json!({ "id": id })).map(drop)
    }

    /// Focus the agent that most needs you; `None` if nothing does.
    pub fn focus_next(&mut self) -> Result<Option<Agent>> {
        Ok(serde_json::from_value(self.call("next", Value::Null)?)?)
    }

    pub fn close(&mut self, id: &str) -> Result<()> {
        self.call("close", json!({ "id": id })).map(drop)
    }

    pub fn send_text(&mut self, id: &str, text: &str, enter: bool) -> Result<()> {
        self.call(
            "send_text",
            json!({ "id": id, "text": text, "enter": enter }),
        )
        .map(drop)
    }

    pub fn get_text(&mut self, id: &str, extent: &str) -> Result<String> {
        Ok(serde_json::from_value(self.call(
            "get_text",
            json!({ "id": id, "extent": extent }),
        )?)?)
    }

    pub fn rename(&mut self, id: &str, name: &str) -> Result<()> {
        self.call("rename", json!({ "id": id, "name": name }))
            .map(drop)
    }

    pub fn forget(&mut self, id: &str) -> Result<u64> {
        Ok(serde_json::from_value(
            self.call("forget", json!({ "id": id }))?,
        )?)
    }

    pub fn forget_exited(&mut self) -> Result<u64> {
        Ok(serde_json::from_value(
            self.call("forget", json!({ "exited": true }))?,
        )?)
    }

    pub fn spawn(&mut self, profile: &str, name: Option<&str>, cwd: Option<&str>) -> Result<Agent> {
        Ok(serde_json::from_value(self.call(
            "spawn",
            json!({ "profile": profile, "name": name, "cwd": cwd }),
        )?)?)
    }
}

/// A `subscribe` stream. Iterate to receive events; the first is always a snapshot.
pub struct Subscription {
    reader: BufReader<UnixStream>,
}

impl Subscription {
    pub fn open() -> Result<Self> {
        Self::open_at(&socket_path())
    }

    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let mut s = UnixStream::connect(path)?;
        s.write_all(b"{\"id\":1,\"method\":\"subscribe\"}\n")?;
        let mut reader = BufReader::new(s);
        let mut ack = String::new();
        reader.read_line(&mut ack)?;
        let v: Value = serde_json::from_str(&ack)?;
        if v["ok"].as_bool() != Some(true) {
            return Err(Error::Daemon(
                v["error"]
                    .as_str()
                    .unwrap_or("subscribe failed")
                    .to_string(),
            ));
        }
        Ok(Subscription { reader })
    }

    /// Next event, or `Ok(None)` when the daemon closed the stream.
    pub fn next_event(&mut self) -> Result<Option<Event>> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            return Ok(Some(serde_json::from_str(&line)?));
        }
    }
}

impl Iterator for Subscription {
    type Item = Result<Event>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_event().transpose()
    }
}

/// Run forever: subscribe, feed events to `on_event`, reconnect with backoff
/// (0.5s → 5s) whenever the daemon is down. `on_disconnect` lets UIs show a
/// "daemon not running" state. Returns only if a callback returns `false`.
pub fn follow(on_event: impl FnMut(Event) -> bool, on_disconnect: impl FnMut(&Error) -> bool) {
    follow_at(&socket_path(), on_event, on_disconnect)
}

/// [`follow`] against an explicit socket path (for `--socket` flags and tests).
pub fn follow_at(
    path: &std::path::Path,
    mut on_event: impl FnMut(Event) -> bool,
    mut on_disconnect: impl FnMut(&Error) -> bool,
) {
    let mut backoff = Duration::from_millis(500);
    loop {
        match Subscription::open_at(path) {
            Ok(sub) => {
                backoff = Duration::from_millis(500);
                for ev in sub {
                    match ev {
                        Ok(ev) => {
                            if !on_event(ev) {
                                return;
                            }
                        }
                        Err(e) => {
                            if !on_disconnect(&e) {
                                return;
                            }
                            break;
                        }
                    }
                }
                let eof = Error::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "daemon closed stream",
                ));
                if !on_disconnect(&eof) {
                    return;
                }
            }
            Err(e) => {
                if !on_disconnect(&e) {
                    return;
                }
            }
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, status: Status, attention: bool, updated_at: u64) -> Agent {
        serde_json::from_value(json!({
            "id": id, "name": id, "kind": "claude", "status": status,
            "attention": attention, "updated_at": updated_at, "created_at": 0,
            "some_future_field": 42
        }))
        .unwrap()
    }

    #[test]
    fn parses_daemon_agent_and_ignores_unknown_fields() {
        let a = agent("a", Status::NeedsInput, true, 1);
        assert_eq!(a.status, Status::NeedsInput);
        assert!(!a.maybe);
    }

    #[test]
    fn events_round_trip() {
        let ev: Event = serde_json::from_str(r#"{"event":"removed","id":"x"}"#).unwrap();
        assert_eq!(ev, Event::Removed { id: "x".into() });
        let ev: Event = serde_json::from_str(r#"{"event":"snapshot","agents":[]}"#).unwrap();
        assert_eq!(ev, Event::Snapshot { agents: vec![] });
    }

    #[test]
    fn apply_and_sort() {
        let mut v = vec![];
        apply(
            &mut v,
            Event::Snapshot {
                agents: vec![agent("a", Status::Working, false, 5)],
            },
        );
        apply(
            &mut v,
            Event::Agent {
                agent: Box::new(agent("b", Status::Idle, true, 1)),
            },
        );
        apply(
            &mut v,
            Event::Agent {
                agent: Box::new(agent("c", Status::NeedsInput, false, 0)),
            },
        );
        apply(
            &mut v,
            Event::Agent {
                agent: Box::new(agent("a", Status::Idle, false, 9)),
            },
        );
        sort_agents(&mut v);
        let ids: Vec<_> = v.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["c", "b", "a"]);
        apply(&mut v, Event::Removed { id: "b".into() });
        assert_eq!(v.len(), 2);
    }
}
