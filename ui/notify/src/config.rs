//! Optional TOML configuration (`~/.config/drove/notify.toml`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::decide::Transition;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Global quiet mode: no notifications and no sounds (closes still happen).
    pub quiet: bool,
    /// Minimum seconds between notifications for the same agent (0 = off).
    pub rate_limit_secs: f64,
    /// Notification icon (theme name or absolute path).
    pub icon: String,
    /// Expiry in ms for non-critical notifications (0 = server default).
    pub timeout_ms: u32,
    pub transitions: Transitions,
    pub sound: Sounds,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Transitions {
    pub needs_input: bool,
    pub finished: bool,
    pub exited: bool,
}

/// Shell commands (run with `sh -c`) per transition; empty = none.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Sounds {
    pub needs_input: String,
    pub finished: String,
    pub exited: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            quiet: false,
            rate_limit_secs: 10.0,
            icon: "utilities-terminal".into(),
            timeout_ms: 0,
            transitions: Transitions::default(),
            sound: Sounds::default(),
        }
    }
}

impl Default for Transitions {
    fn default() -> Self {
        Transitions {
            needs_input: true,
            finished: true,
            exited: true,
        }
    }
}

impl Transitions {
    pub fn enabled(&self, t: Transition) -> bool {
        match t {
            Transition::NeedsInput => self.needs_input,
            Transition::Finished => self.finished,
            Transition::Exited => self.exited,
        }
    }
}

impl Sounds {
    pub fn command(&self, t: Transition) -> Option<&str> {
        let c = match t {
            Transition::NeedsInput => &self.needs_input,
            Transition::Finished => &self.finished,
            Transition::Exited => &self.exited,
        };
        (!c.trim().is_empty()).then_some(c.as_str())
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        Ok(toml::from_str(text)?)
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Load `path` if given (must exist), else the default location if present,
    /// else defaults.
    pub fn load_or_default(path: Option<&Path>) -> Result<Config> {
        match path {
            Some(p) => Self::load(p),
            None => match default_path() {
                Some(p) if p.exists() => Self::load(&p),
                _ => Ok(Config::default()),
            },
        }
    }
}

pub fn default_path() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|s| !s.is_empty());
    let base = match env("XDG_CONFIG_HOME") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env("HOME")?).join(".config"),
    };
    Some(base.join("drove").join("notify.toml"))
}

/// `$XDG_RUNTIME_DIR/drove/quiet`, else next to the socket.
pub fn quiet_file_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()) {
        Some(d) => PathBuf::from(d).join("drove").join("quiet"),
        None => drove_client::socket_path().with_file_name("quiet"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn partial_override() {
        let c = Config::parse(
            r#"
            quiet = true
            rate_limit_secs = 2.5
            [transitions]
            exited = false
            [sound]
            needs_input = "pw-play /x.oga"
            "#,
        )
        .unwrap();
        assert!(c.quiet);
        assert_eq!(c.rate_limit_secs, 2.5);
        assert!(c.transitions.needs_input && !c.transitions.exited);
        assert_eq!(
            c.sound.command(Transition::NeedsInput),
            Some("pw-play /x.oga")
        );
        assert_eq!(c.sound.command(Transition::Finished), None);
    }

    #[test]
    fn unknown_key_is_ok_but_bad_type_errors() {
        assert!(Config::parse("future = 1").is_ok());
        assert!(Config::parse("quiet = 3").is_err());
    }
}
