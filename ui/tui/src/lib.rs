//! drove-tui: terminal dashboard for the drove daemon.
//!
//! * [`app`]    pure state + key handling (`key -> Vec<Action>`), no I/O
//! * [`ui`]     ratatui rendering of an [`app::App`]
//! * [`worker`] threads: event follower, action executor, preview poller

pub mod app;
pub mod ui;
pub mod worker;
