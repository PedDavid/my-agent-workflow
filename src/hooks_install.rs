//! Hook snippets for each agent, and idempotent installation into their config files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::shell;

pub const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionEnd",
];

pub const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PreCompact",
    "Stop",
    "Interrupt",
    "SessionEnd",
];

pub const KIRO_EVENTS: &[&str] = &[
    "agentSpawn",
    "userPromptSubmit",
    "preToolUse",
    "postToolUse",
    "stop",
];

/// Shell command line the agent runs for each hook.
pub fn hook_command(drove: &str, source: &str) -> String {
    format!("{} hook {source}", shell::quote(drove))
}

fn is_ours(cmd: &str, source: &str) -> bool {
    let suffix = format!(" hook {source}");
    cmd.ends_with(&suffix) && cmd[..cmd.len() - suffix.len()].contains("drove")
}

/// Claude/Codex `hooks` object: `{Event: [{matcher, hooks: [{type, command}]}]}`.
pub fn claude_like_hooks(drove: &str, source: &str, events: &[&str]) -> Value {
    let cmd = hook_command(drove, source);
    let mut m = Map::new();
    for e in events {
        m.insert(
            e.to_string(),
            json!([{"matcher": "", "hooks": [{"type": "command", "command": cmd, "timeout": 5}]}]),
        );
    }
    Value::Object(m)
}

/// Contents of the `--settings` file drove passes to spawned Claude Code sessions.
pub fn claude_settings(drove: &str) -> Value {
    json!({"hooks": claude_like_hooks(drove, "claude", CLAUDE_EVENTS)})
}

pub fn codex_hooks_file(drove: &str) -> Value {
    json!({"hooks": claude_like_hooks(drove, "codex", CODEX_EVENTS)})
}

/// kiro agent-config `hooks` object.
pub fn kiro_hooks(drove: &str) -> Value {
    let cmd = hook_command(drove, "kiro");
    let mut m = Map::new();
    for e in KIRO_EVENTS {
        let entry = if e.ends_with("ToolUse") {
            json!({"matcher": "*", "command": cmd})
        } else {
            json!({"command": cmd})
        };
        m.insert(e.to_string(), json!([entry]));
    }
    Value::Object(m)
}

/// Merge Claude/Codex style hooks into `root["hooks"]`, replacing any earlier
/// drove entries for `source`. Returns true if anything changed.
pub fn merge_claude_like(root: &mut Value, ours: &Value, source: &str) -> Result<bool> {
    let before = root.clone();
    if !root.is_object() {
        bail!("config root is not a JSON object");
    }
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        bail!("`hooks` is not an object");
    };
    for (event, groups) in ours.as_object().unwrap() {
        let list = hooks.entry(event.clone()).or_insert_with(|| json!([]));
        let Some(list) = list.as_array_mut() else {
            bail!("hooks.{event} is not an array");
        };
        // Drop our old handlers (and groups left empty by that).
        for g in list.iter_mut() {
            if let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                hs.retain(|h| {
                    !h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| is_ours(c, source))
                });
            }
        }
        list.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|h| !h.is_empty())
        });
        list.extend(groups.as_array().unwrap().iter().cloned());
    }
    Ok(*root != before)
}

/// Merge kiro hooks into an agent config (`root["hooks"]`).
pub fn merge_kiro(root: &mut Value, ours: &Value) -> Result<bool> {
    let before = root.clone();
    if !root.is_object() {
        bail!("agent config is not a JSON object");
    }
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        bail!("`hooks` is not an object");
    };
    for (event, entries) in ours.as_object().unwrap() {
        let list = hooks.entry(event.clone()).or_insert_with(|| json!([]));
        let Some(list) = list.as_array_mut() else {
            bail!("hooks.{event} is not an array");
        };
        list.retain(|h| {
            !h.get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| is_ours(c, "kiro"))
        });
        list.extend(entries.as_array().unwrap().iter().cloned());
    }
    Ok(*root != before)
}

/// Read JSON (missing file = `{}`), apply `f`, and if it changed write it back
/// after copying the original to `<file>.drove-bak`. Returns whether it changed.
pub fn edit_json_file(path: &Path, f: impl FnOnce(&mut Value) -> Result<bool>) -> Result<bool> {
    let original = match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut root: Value = match &original {
        Some(s) if !s.trim().is_empty() => {
            serde_json::from_str(s).with_context(|| format!("parsing {}", path.display()))?
        }
        _ => json!({}),
    };
    if !f(&mut root)? {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(orig) = &original {
        let bak = PathBuf::from(format!("{}.drove-bak", path.display()));
        std::fs::write(&bak, orig).with_context(|| format!("writing {}", bak.display()))?;
    }
    let tmp = PathBuf::from(format!("{}.drove-tmp", path.display()));
    std::fs::write(&tmp, serde_json::to_string_pretty(&root)? + "\n")?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

pub fn codex_hooks_path() -> PathBuf {
    let base = std::env::var("CODEX_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::paths::home().join(".codex"));
    base.join("hooks.json")
}

pub fn claude_settings_path() -> PathBuf {
    crate::paths::home().join(".claude/settings.json")
}

pub fn kiro_agents_dir() -> PathBuf {
    crate::paths::home().join(".kiro/agents")
}

pub fn install_codex(drove: &str, path: &Path) -> Result<bool> {
    let ours = claude_like_hooks(drove, "codex", CODEX_EVENTS);
    edit_json_file(path, |root| merge_claude_like(root, &ours, "codex"))
}

pub fn install_claude(drove: &str, path: &Path) -> Result<bool> {
    let ours = claude_like_hooks(drove, "claude", CLAUDE_EVENTS);
    edit_json_file(path, |root| merge_claude_like(root, &ours, "claude"))
}

/// Install into one agent file, or every `*.json` in `dir` when `agent` is None.
pub fn install_kiro(drove: &str, dir: &Path, agent: Option<&str>) -> Result<Vec<(PathBuf, bool)>> {
    let ours = kiro_hooks(drove);
    let files: Vec<PathBuf> = match agent {
        Some(a) => vec![dir.join(format!("{a}.json"))],
        None => {
            let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
                .with_context(|| {
                    format!(
                        "reading {} (pass --agent NAME to create one)",
                        dir.display()
                    )
                })?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect();
            v.sort();
            if v.is_empty() {
                bail!(
                    "no agent configs in {} (pass --agent NAME to create one)",
                    dir.display()
                );
            }
            v
        }
    };
    files
        .into_iter()
        .map(|p| {
            let changed = edit_json_file(&p, |root| merge_kiro(root, &ours))?;
            Ok((p, changed))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str = "/usr/bin/drove";

    #[test]
    fn claude_settings_shape() {
        let s = claude_settings(D);
        let stop = &s["hooks"]["Stop"][0];
        assert_eq!(stop["matcher"], "");
        assert_eq!(stop["hooks"][0]["type"], "command");
        assert_eq!(stop["hooks"][0]["command"], "/usr/bin/drove hook claude");
        assert_eq!(s["hooks"].as_object().unwrap().len(), CLAUDE_EVENTS.len());
        assert_eq!(
            hook_command("/home/me/my bin/drove", "kiro"),
            "'/home/me/my bin/drove' hook kiro"
        );
    }

    #[test]
    fn codex_merge_is_idempotent_and_keeps_user_hooks() {
        let mut root = json!({
            "description": "mine",
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}],
                "PreToolUse": [{"matcher": "shell", "hooks": [
                    {"type": "command", "command": "/old/path/drove hook codex"},
                    {"type": "command", "command": "audit"}
                ]}]
            }
        });
        let ours = claude_like_hooks(D, "codex", CODEX_EVENTS);
        assert!(merge_claude_like(&mut root, &ours, "codex").unwrap());
        let snapshot = root.clone();
        assert!(!merge_claude_like(&mut root, &ours, "codex").unwrap());
        assert_eq!(root, snapshot);
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "notify-send done");
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(
            pre[0]["hooks"].as_array().unwrap().len(),
            1,
            "old drove entry removed"
        );
        assert_eq!(pre[1]["hooks"][0]["command"], "/usr/bin/drove hook codex");
        assert_eq!(root["description"], "mine");
    }

    #[test]
    fn kiro_merge() {
        let mut root = json!({"name": "dev", "hooks": {"stop": [{"command": "say hi"}]}});
        let ours = kiro_hooks(D);
        assert!(merge_kiro(&mut root, &ours).unwrap());
        assert!(!merge_kiro(&mut root, &ours).unwrap());
        assert_eq!(root["hooks"]["stop"].as_array().unwrap().len(), 2);
        assert_eq!(root["hooks"]["preToolUse"][0]["matcher"], "*");
        assert_eq!(root["name"], "dev");
    }

    #[test]
    fn file_install_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hooks.json");
        // missing file is created, no backup
        assert!(install_codex(D, &p).unwrap());
        assert!(!dir.path().join("hooks.json.drove-bak").exists());
        assert!(!install_codex(D, &p).unwrap());
        // existing file gets a backup
        std::fs::write(&p, r#"{"hooks":{}}"#).unwrap();
        assert!(install_codex(D, &p).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hooks.json.drove-bak")).unwrap(),
            r#"{"hooks":{}}"#
        );
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v, codex_hooks_file(D));
        // garbage is refused, untouched
        std::fs::write(&p, "not json").unwrap();
        assert!(install_codex(D, &p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "not json");
    }

    #[test]
    fn kiro_dir_install() {
        let dir = tempfile::tempdir().unwrap();
        assert!(install_kiro(D, dir.path(), None).is_err());
        std::fs::write(dir.path().join("a.json"), r#"{"name":"a"}"#).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let r = install_kiro(D, dir.path(), None).unwrap();
        assert_eq!(r.len(), 1);
        assert!(r[0].1);
        let r = install_kiro(D, dir.path(), Some("b")).unwrap();
        assert!(r[0].1);
        assert!(dir.path().join("b.json").exists());
    }
}
