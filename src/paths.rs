//! XDG paths and environment overrides.

use std::path::PathBuf;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

pub fn home() -> PathBuf {
    env("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `$XDG_RUNTIME_DIR/drove`, falling back to `/tmp/drove-<uid>`.
pub fn runtime_dir() -> PathBuf {
    match env("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("drove"),
        None => PathBuf::from(format!("/tmp/drove-{}", unsafe { libc::getuid() })),
    }
}

/// Daemon socket (`DROVE_SOCKET` overrides).
pub fn socket_path() -> PathBuf {
    env("DROVE_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_dir().join("drove.sock"))
}

pub fn kitty_socket_path(id: &str) -> PathBuf {
    runtime_dir().join(format!("kitty-{id}.sock"))
}

pub fn state_dir() -> PathBuf {
    if let Some(d) = env("DROVE_STATE_DIR") {
        return PathBuf::from(d);
    }
    env("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("drove")
}

pub fn state_file() -> PathBuf {
    state_dir().join("state.json")
}

pub fn data_dir() -> PathBuf {
    if let Some(d) = env("DROVE_DATA_DIR") {
        return PathBuf::from(d);
    }
    env("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("drove")
}

pub fn config_path() -> PathBuf {
    if let Some(p) = env("DROVE_CONFIG") {
        return PathBuf::from(p);
    }
    env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("drove/config.toml")
}

/// Hyprland event socket for the current instance.
pub fn hypr_socket2() -> Option<PathBuf> {
    let sig = env("HYPRLAND_INSTANCE_SIGNATURE")?;
    let rt = env("XDG_RUNTIME_DIR")?;
    Some(
        PathBuf::from(rt)
            .join("hypr")
            .join(sig)
            .join(".socket2.sock"),
    )
}

pub fn under_hyprland() -> bool {
    env("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}
