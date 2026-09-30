//! `~/.config/drove/config.toml`. Everything is optional.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::AgentKind;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub terminal: TerminalConfig,
    pub hyprland: HyprlandConfig,
    pub picker: PickerConfig,
    pub spawn: SpawnConfig,
    pub kiro: KiroConfig,
    pub hooks: HooksConfig,
    /// Agent profiles, merged over the built-in `claude`, `codex`, `kiro` and `shell`.
    pub agents: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalMode {
    /// One kitty process per agent, each with its own remote-control socket.
    #[default]
    Standalone,
    /// One OS window per agent inside an already running kitty (`socket`).
    Instance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    pub kind: String,
    pub mode: TerminalMode,
    pub kitty: String,
    pub kitten: String,
    /// Remote-control address of the shared kitty (instance mode), e.g. `unix:@mykitty`.
    pub socket: Option<String>,
    /// Extra arguments passed to `kitty` in standalone mode (before the command).
    pub extra_args: Vec<String>,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        TerminalConfig {
            kind: "kitty".into(),
            mode: TerminalMode::Standalone,
            kitty: "kitty".into(),
            kitten: "kitten".into(),
            socket: None,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DispatchMode {
    #[default]
    Auto,
    Lua,
    Legacy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HyprlandConfig {
    pub hyprctl: String,
    pub dispatch: DispatchMode,
    /// Override the event socket path (normally derived from `HYPRLAND_INSTANCE_SIGNATURE`).
    /// Setting it also forces "running under Hyprland" behaviour.
    pub socket2: Option<PathBuf>,
}

impl Default for HyprlandConfig {
    fn default() -> Self {
        HyprlandConfig {
            hyprctl: "hyprctl".into(),
            dispatch: DispatchMode::Auto,
            socket2: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PickerConfig {
    pub command: Vec<String>,
}

impl Default for PickerConfig {
    fn default() -> Self {
        PickerConfig {
            command: vec!["fuzzel".into(), "--dmenu".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpawnConfig {
    /// Hyprland workspace rule for new agent windows, e.g. `"special:agents"` or `"3 silent"`.
    pub workspace: String,
    /// Where `--worktree BRANCH` creates worktrees. Placeholders: `{repo}`, `{repo_name}`, `{branch}`.
    pub worktree_root: String,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        SpawnConfig {
            workspace: String::new(),
            worktree_root: "{repo}/../{repo_name}.worktrees/{branch}".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KiroConfig {
    /// A `preToolUse` without `postToolUse` for this long is reported as needs_input (maybe).
    pub stale_tool_secs: u64,
}

impl Default for KiroConfig {
    fn default() -> Self {
        KiroConfig { stale_tool_secs: 4 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HooksConfig {
    /// Absolute path of the `drove` binary written into hook commands (default: current exe).
    pub drove: Option<String>,
    /// Track agents started outside drove by matching hook ancestor PIDs to windows.
    pub adopt: bool,
}

impl Default for HooksConfig {
    fn default() -> Self {
        HooksConfig {
            drove: None,
            adopt: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default)]
    pub kind: AgentKind,
    pub command: Vec<String>,
    /// Per-profile workspace rule (overrides `spawn.workspace`).
    #[serde(default)]
    pub workspace: Option<String>,
    /// Extra environment for the agent process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Codex only: inject `-c notify=[…]` (default true).
    #[serde(default = "yes")]
    pub inject: bool,
}

fn yes() -> bool {
    true
}

impl Profile {
    fn builtin(kind: AgentKind, command: &[&str]) -> Self {
        Profile {
            kind,
            command: command.iter().map(|s| s.to_string()).collect(),
            workspace: None,
            env: BTreeMap::new(),
            inject: true,
        }
    }
}

pub fn builtin_profiles() -> BTreeMap<String, Profile> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
    let mut m = BTreeMap::new();
    m.insert(
        "claude".into(),
        Profile::builtin(AgentKind::Claude, &["claude"]),
    );
    m.insert(
        "codex".into(),
        Profile::builtin(AgentKind::Codex, &["codex"]),
    );
    m.insert(
        "kiro".into(),
        Profile::builtin(AgentKind::Kiro, &["kiro-cli", "chat"]),
    );
    m.insert(
        "shell".into(),
        Profile::builtin(AgentKind::Generic, &[shell.as_str()]),
    );
    m
}

impl Config {
    pub fn parse(s: &str) -> Result<Self> {
        let mut cfg: Config = toml::from_str(s)?;
        let mut agents = builtin_profiles();
        agents.append(&mut cfg.agents);
        cfg.agents = agents;
        Ok(cfg)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::parse(&s).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::parse(""),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&crate::paths::config_path())
    }

    /// Absolute path of the drove binary to put in hook commands.
    pub fn drove_bin(&self) -> String {
        if let Some(p) = &self.hooks.drove {
            return p.clone();
        }
        std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "drove".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_has_builtins() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.agents["claude"].command, vec!["claude"]);
        assert_eq!(c.agents["kiro"].command, vec!["kiro-cli", "chat"]);
        assert_eq!(c.agents["codex"].kind, AgentKind::Codex);
        assert_eq!(c.picker.command, vec!["fuzzel", "--dmenu"]);
        assert_eq!(c.hyprland.dispatch, DispatchMode::Auto);
        assert_eq!(c.kiro.stale_tool_secs, 4);
        assert!(c.hooks.adopt);
    }

    #[test]
    fn user_profiles_merge() {
        let c = Config::parse(
            r#"
            [terminal]
            mode = "instance"
            socket = "unix:@mykitty"
            [hyprland]
            dispatch = "legacy"
            hyprctl = "/tmp/fake"
            [spawn]
            workspace = "special:agents"
            [agents.opus]
            kind = "claude"
            command = ["claude", "--model", "opus"]
            workspace = "3 silent"
            [agents.claude]
            kind = "claude"
            command = ["/opt/claude"]
            "#,
        )
        .unwrap();
        assert_eq!(c.terminal.mode, TerminalMode::Instance);
        assert_eq!(c.hyprland.dispatch, DispatchMode::Legacy);
        assert_eq!(c.agents["opus"].workspace.as_deref(), Some("3 silent"));
        assert_eq!(c.agents["claude"].command, vec!["/opt/claude"]);
        assert!(c.agents.contains_key("codex"));
        assert_eq!(c.spawn.workspace, "special:agents");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("[terminal]\nmoed = \"x\"").is_err());
    }
    #[test]
    fn contrib_config_parses() {
        let c = Config::parse(include_str!("../contrib/config.toml")).unwrap();
        assert_eq!(c.terminal.mode, TerminalMode::Standalone);
    }
}
