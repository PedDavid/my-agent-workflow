//! Core data model shared by the daemon, the protocol and the CLI.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Starting => "starting",
            Status::Idle => "idle",
            Status::Working => "working",
            Status::NeedsInput => "needs_input",
            Status::Exited => "exited",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Kiro,
    #[default]
    Generic,
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Kiro => "kiro",
            AgentKind::Generic => "generic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WindowRef {
    /// Hyprland address including the `0x` prefix.
    pub address: String,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub class: String,
}

/// Handle on the terminal that hosts the agent. An enum so other terminals
/// can be added later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TermRef {
    Kitty {
        /// Remote-control socket, e.g. `unix:/run/user/1000/drove/kitty-abc123.sock`.
        socket: String,
        /// Kitty window id (instance mode only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_id: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub kind: AgentKind,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub cwd: String,
    pub status: Status,
    #[serde(default)]
    pub attention: bool,
    #[serde(default)]
    pub detail: String,
    /// True when the status is a guess (kiro stale tool heuristic).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub maybe: bool,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub window: Option<WindowRef>,
    #[serde(default)]
    pub term: Option<TermRef>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub adopted: bool,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub last_hook_at: Option<u64>,
    /// Kiro only: when the currently running tool started (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_tool_since: Option<u64>,
}

impl Agent {
    pub fn new(id: &str, name: &str, kind: AgentKind, profile: &str, cwd: &str, now: u64) -> Self {
        Agent {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            profile: profile.to_string(),
            cwd: cwd.to_string(),
            status: Status::Starting,
            attention: false,
            detail: String::new(),
            maybe: false,
            session_id: None,
            window: None,
            term: None,
            worktree: None,
            adopted: false,
            created_at: now,
            updated_at: now,
            last_hook_at: None,
            pending_tool_since: None,
        }
    }
}

/// Generate a random 6 char lowercase base32 id.
pub fn new_id() -> String {
    use std::io::Read;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut buf = [0u8; 6];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !ok {
        let n = crate::now_ms() ^ (std::process::id() as u64) << 20;
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((n >> (i * 5)) & 0xff) as u8;
        }
    }
    buf.iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

/// First line of `s`, trimmed and truncated to `max` chars.
pub fn snippet(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > max {
        let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_shape() {
        let id = new_id();
        assert_eq!(id.len(), 6);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c))
        );
    }

    #[test]
    fn snippet_truncates() {
        assert_eq!(snippet("\n  hello world\nsecond", 100), "hello world");
        assert_eq!(snippet("abcdefghij", 5), "abcd…");
        assert_eq!(snippet("", 5), "");
    }

    #[test]
    fn status_serde() {
        assert_eq!(
            serde_json::to_string(&Status::NeedsInput).unwrap(),
            "\"needs_input\""
        );
    }
}
