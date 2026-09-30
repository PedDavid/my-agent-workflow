//! drove: agent herding built into the window manager.

pub mod adapters;
pub mod config;
pub mod lua;
pub mod model;
pub mod paths;
pub mod shell;
pub mod state;
pub mod term;
pub mod wm;

/// Current unix time in milliseconds.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
