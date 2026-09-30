//! drove: agent herding built into the window manager.

pub mod adapters;
pub mod cli;
pub mod client;
pub mod config;
pub mod daemon;
pub mod hooks_install;
pub mod lua;
pub mod model;
pub mod paths;
pub mod protocol;
pub mod shell;
pub mod spawn;
pub mod state;
pub mod term;
pub mod ui;
pub mod wm;

/// Current unix time in milliseconds.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
