//! Normalise agent hook payloads into [`Signal`]s. Pure functions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{AgentKind, snippet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Claude,
    Codex,
    Kiro,
    CodexNotify,
}

impl Source {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "claude" => Source::Claude,
            "codex" => Source::Codex,
            "kiro" => Source::Kiro,
            "codex-notify" => Source::CodexNotify,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Claude => "claude",
            Source::Codex => "codex",
            Source::Kiro => "kiro",
            Source::CodexNotify => "codex-notify",
        }
    }

    pub fn kind(&self) -> AgentKind {
        match self {
            Source::Claude => AgentKind::Claude,
            Source::Codex | Source::CodexNotify => AgentKind::Codex,
            Source::Kiro => AgentKind::Kiro,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum Signal {
    SessionStarted,
    PromptSubmitted {
        prompt: String,
    },
    ToolStart {
        name: String,
    },
    ToolEnd {
        name: String,
    },
    NeedsInput {
        msg: String,
    },
    TurnDone {
        last_message: Option<String>,
    },
    /// The agent is sitting idle at its prompt (e.g. Claude's `idle_prompt`
    /// reminder). Unlike `TurnDone` it does not raise attention.
    Idle,
    SessionEnded,
    Ignore,
}

/// A normalised hook: the signal plus whatever identity the payload carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub signal: Signal,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

const SNIPPET: usize = 80;

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn owned(v: &Value, key: &str) -> Option<String> {
    s(v, key).map(str::to_string).filter(|x| !x.is_empty())
}

fn tool(v: &Value) -> String {
    s(v, "tool_name").unwrap_or("tool").to_string()
}

pub fn normalize(source: Source, payload: &Value) -> Hook {
    let signal = match source {
        Source::Claude | Source::Codex => claude_like(payload),
        Source::Kiro => kiro(payload),
        Source::CodexNotify => codex_notify(payload),
    };
    let session_id = match source {
        Source::CodexNotify => owned(payload, "thread-id"),
        Source::Kiro => None,
        _ => owned(payload, "session_id"),
    };
    Hook {
        signal,
        session_id,
        cwd: owned(payload, "cwd"),
    }
}

fn claude_like(v: &Value) -> Signal {
    match s(v, "hook_event_name").unwrap_or("") {
        "SessionStart" => Signal::SessionStarted,
        "UserPromptSubmit" => Signal::PromptSubmitted {
            prompt: snippet(s(v, "prompt").unwrap_or(""), SNIPPET),
        },
        "PreToolUse" => Signal::ToolStart { name: tool(v) },
        "PostToolUse" | "PostToolUseFailure" => Signal::ToolEnd { name: tool(v) },
        "PreCompact" => Signal::ToolStart {
            name: "compact".into(),
        },
        "PermissionRequest" => Signal::NeedsInput {
            msg: format!("permission: {}", tool(v)),
        },
        "Notification" => notification(v),
        "Stop" | "Interrupt" => Signal::TurnDone {
            last_message: s(v, "last_assistant_message").map(|m| snippet(m, SNIPPET)),
        },
        "SessionEnd" => Signal::SessionEnded,
        // SubagentStart/SubagentStop/PostCompact and anything new: no state change.
        _ => Signal::Ignore,
    }
}

fn notification(v: &Value) -> Signal {
    let msg = s(v, "message").unwrap_or("").to_string();
    match s(v, "notification_type") {
        Some("idle_prompt") => Signal::Idle,
        Some("auth_success") => Signal::Ignore,
        Some(_) => Signal::NeedsInput {
            msg: snippet(&msg, SNIPPET),
        },
        // Older versions only send a message.
        None if msg.contains("waiting for your input") => Signal::Idle,
        None => Signal::NeedsInput {
            msg: snippet(&msg, SNIPPET),
        },
    }
}

fn kiro(v: &Value) -> Signal {
    match s(v, "hook_event_name").unwrap_or("") {
        "agentSpawn" => Signal::SessionStarted,
        "userPromptSubmit" => Signal::PromptSubmitted {
            prompt: snippet(s(v, "prompt").unwrap_or(""), SNIPPET),
        },
        "preToolUse" => Signal::ToolStart { name: tool(v) },
        "postToolUse" => Signal::ToolEnd { name: tool(v) },
        "stop" => Signal::TurnDone { last_message: None },
        _ => Signal::Ignore,
    }
}

fn codex_notify(v: &Value) -> Signal {
    match s(v, "type") {
        Some("agent-turn-complete") => Signal::TurnDone {
            last_message: s(v, "last-assistant-message").map(|m| snippet(m, SNIPPET)),
        },
        _ => Signal::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sig(src: Source, v: Value) -> Signal {
        normalize(src, &v).signal
    }

    #[test]
    fn claude_session_start() {
        let h = normalize(
            Source::Claude,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "abc-123",
                "transcript_path": "/home/u/.claude/projects/x/abc-123.jsonl",
                "cwd": "/home/u/src/proj",
                "source": "startup"
            }),
        );
        assert_eq!(h.signal, Signal::SessionStarted);
        assert_eq!(h.session_id.as_deref(), Some("abc-123"));
        assert_eq!(h.cwd.as_deref(), Some("/home/u/src/proj"));
    }

    #[test]
    fn claude_prompt_and_tools() {
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"fix the\nbug"})
            ),
            Signal::PromptSubmitted {
                prompt: "fix the".into()
            }
        );
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"PreToolUse","session_id":"s","tool_name":"Bash","tool_input":{"command":"ls"}})
            ),
            Signal::ToolStart {
                name: "Bash".into()
            }
        );
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"PostToolUse","tool_name":"Edit","tool_response":{}})
            ),
            Signal::ToolEnd {
                name: "Edit".into()
            }
        );
    }

    #[test]
    fn claude_permission_and_notifications() {
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"PermissionRequest","tool_name":"Bash"})
            ),
            Signal::NeedsInput {
                msg: "permission: Bash".into()
            }
        );
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"})
            ),
            Signal::NeedsInput {
                msg: "Claude needs your permission to use Bash".into()
            }
        );
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"Notification","notification_type":"idle_prompt","message":"Claude is waiting for your input"})
            ),
            Signal::Idle
        );
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"Notification","message":"Claude is waiting for your input"})
            ),
            Signal::Idle
        );
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"Notification","notification_type":"auth_success","message":"ok"})
            ),
            Signal::Ignore
        );
    }

    #[test]
    fn claude_stop_and_end() {
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"Stop","session_id":"s","stop_hook_active":false})
            ),
            Signal::TurnDone { last_message: None }
        );
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"SubagentStop","session_id":"s"})
            ),
            Signal::Ignore
        );
        assert_eq!(
            sig(
                Source::Claude,
                json!({"hook_event_name":"SessionEnd","reason":"exit"})
            ),
            Signal::SessionEnded
        );
        assert_eq!(sig(Source::Claude, json!({})), Signal::Ignore);
        assert_eq!(sig(Source::Claude, json!("garbage")), Signal::Ignore);
    }

    #[test]
    fn codex_hooks() {
        let h = normalize(
            Source::Codex,
            &json!({
                "hook_event_name": "Stop",
                "session_id": "019a-thread",
                "turn_id": "t1",
                "cwd": "/w",
                "last_assistant_message": "All done.\nDetails…"
            }),
        );
        assert_eq!(
            h.signal,
            Signal::TurnDone {
                last_message: Some("All done.".into())
            }
        );
        assert_eq!(h.session_id.as_deref(), Some("019a-thread"));
        assert_eq!(
            sig(
                Source::Codex,
                json!({"hook_event_name":"PermissionRequest","tool_name":"shell","tool_input":{}})
            ),
            Signal::NeedsInput {
                msg: "permission: shell".into()
            }
        );
        assert_eq!(
            sig(Source::Codex, json!({"hook_event_name":"PreCompact"})),
            Signal::ToolStart {
                name: "compact".into()
            }
        );
        assert_eq!(
            sig(Source::Codex, json!({"hook_event_name":"PostCompact"})),
            Signal::Ignore
        );
        assert_eq!(
            sig(Source::Codex, json!({"hook_event_name":"SubagentStart"})),
            Signal::Ignore
        );
    }

    #[test]
    fn codex_notify_turn_complete() {
        let h = normalize(
            Source::CodexNotify,
            &json!({
                "type": "agent-turn-complete",
                "thread-id": "th-1",
                "turn-id": "tu-1",
                "cwd": "/w",
                "input-messages": ["do it"],
                "last-assistant-message": "Done!"
            }),
        );
        assert_eq!(
            h.signal,
            Signal::TurnDone {
                last_message: Some("Done!".into())
            }
        );
        assert_eq!(h.session_id.as_deref(), Some("th-1"));
        assert_eq!(
            sig(Source::CodexNotify, json!({"type":"something-else"})),
            Signal::Ignore
        );
    }

    #[test]
    fn kiro_hooks() {
        let h = normalize(
            Source::Kiro,
            &json!({"hook_event_name":"agentSpawn","cwd":"/k"}),
        );
        assert_eq!(h.signal, Signal::SessionStarted);
        assert_eq!(h.session_id, None);
        assert_eq!(
            sig(
                Source::Kiro,
                json!({"hook_event_name":"userPromptSubmit","cwd":"/k","prompt":"hello"})
            ),
            Signal::PromptSubmitted {
                prompt: "hello".into()
            }
        );
        assert_eq!(
            sig(
                Source::Kiro,
                json!({"hook_event_name":"preToolUse","cwd":"/k","tool_name":"execute_bash","tool_input":{"command":"ls"}})
            ),
            Signal::ToolStart {
                name: "execute_bash".into()
            }
        );
        assert_eq!(
            sig(
                Source::Kiro,
                json!({"hook_event_name":"postToolUse","tool_name":"fs_read","tool_response":{"success":true}})
            ),
            Signal::ToolEnd {
                name: "fs_read".into()
            }
        );
        assert_eq!(
            sig(Source::Kiro, json!({"hook_event_name":"stop","cwd":"/k"})),
            Signal::TurnDone { last_message: None }
        );
    }

    #[test]
    fn source_roundtrip() {
        for s in ["claude", "codex", "kiro", "codex-notify"] {
            assert_eq!(Source::parse(s).unwrap().as_str(), s);
        }
        assert!(Source::parse("x").is_none());
    }
}
