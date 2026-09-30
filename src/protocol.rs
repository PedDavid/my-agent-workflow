//! NDJSON protocol between clients and the daemon.
//!
//! Request:  `{"id":1,"method":"list","params":{}}`
//! Response: `{"id":1,"ok":true,"result":…}` or `{"id":1,"ok":false,"error":"…"}`
//! After `subscribe` the connection becomes a stream of [`Event`]s.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Agent;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub id: Value,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Response {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, error: impl ToString) -> Self {
        Response {
            id,
            ok: false,
            result: None,
            error: Some(error.to_string()),
        }
    }
}

/// Subscription stream events. This is the public API for bars and UIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Always first; also re-sent if the subscriber fell behind.
    Snapshot { agents: Vec<Agent> },
    /// An agent was added or changed (full object).
    Agent { agent: Box<Agent> },
    /// An agent was forgotten.
    Removed { id: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SpawnParams {
    pub profile: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub worktree: Option<String>,
    pub workspace: Option<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HookParams {
    /// `DROVE_AGENT_ID` of the hook's environment, if set.
    pub agent_id: Option<String>,
    pub source: String,
    pub payload: Value,
    /// Ancestor PIDs of the hook process (for adopting agents started by hand).
    pub pids: Vec<i64>,
}

pub const METHODS: &[&str] = &[
    "ping",
    "status",
    "list",
    "get",
    "spawn",
    "focus",
    "next",
    "close",
    "send_text",
    "get_text",
    "rename",
    "forget",
    "hook",
    "subscribe",
    "shutdown",
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_shapes() {
        let r: Request = serde_json::from_str(r#"{"id":1,"method":"list"}"#).unwrap();
        assert_eq!(r.method, "list");
        assert_eq!(r.params, Value::Null);
        let ok = serde_json::to_value(Response::ok(json!(1), json!([]))).unwrap();
        assert_eq!(ok, json!({"id":1,"ok":true,"result":[]}));
        let err = serde_json::to_value(Response::err(json!(2), "nope")).unwrap();
        assert_eq!(err, json!({"id":2,"ok":false,"error":"nope"}));
    }

    #[test]
    fn event_shapes() {
        let e = serde_json::to_value(Event::Removed { id: "abc".into() }).unwrap();
        assert_eq!(e, json!({"event":"removed","id":"abc"}));
        let e = serde_json::to_value(Event::Snapshot { agents: vec![] }).unwrap();
        assert_eq!(e, json!({"event":"snapshot","agents":[]}));
    }
}
