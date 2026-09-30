//! Window-manager layer. Narrow surface (clients/focus/close/exec/events) so
//! other compositors can be added later.

pub mod hyprland;

use serde::{Deserialize, Serialize};

/// A compositor event we care about. Addresses always carry the `0x` prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WmEvent {
    OpenWindow {
        address: String,
        workspace: String,
        class: String,
        title: String,
    },
    CloseWindow {
        address: String,
    },
    /// `None` when nothing is focused.
    ActiveWindow {
        address: Option<String>,
    },
    Title {
        address: String,
        title: String,
    },
    MoveWindow {
        address: String,
        workspace: String,
    },
    Urgent {
        address: String,
    },
}

/// A window as reported by the compositor's client list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Client {
    pub address: String,
    pub class: String,
    pub initial_class: String,
    pub title: String,
    pub pid: i64,
    pub workspace: String,
    pub focus_history_id: i64,
}

/// Class drove gives agent windows.
pub fn agent_class(id: &str) -> String {
    format!("drove-{id}")
}

/// Inverse of [`agent_class`].
pub fn id_from_class(class: &str) -> Option<&str> {
    let id = class.strip_prefix("drove-")?;
    (id.len() == 6
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c)))
    .then_some(id)
}

/// `55d1c0ffee` / `0x55d1c0ffee` → `0x55d1c0ffee`.
pub fn norm_addr(a: &str) -> String {
    let a = a.trim();
    if a.starts_with("0x") {
        a.to_string()
    } else {
        format!("0x{a}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert_eq!(agent_class("ab12cd"), "drove-ab12cd");
        assert_eq!(id_from_class("drove-ab23cd"), Some("ab23cd"));
        assert_eq!(id_from_class("drove-AB"), None);
        assert_eq!(id_from_class("kitty"), None);
        assert_eq!(id_from_class("drove-abc1ef"), None);
    }

    #[test]
    fn addrs() {
        assert_eq!(norm_addr("55d1"), "0x55d1");
        assert_eq!(norm_addr("0x55d1"), "0x55d1");
    }
}
