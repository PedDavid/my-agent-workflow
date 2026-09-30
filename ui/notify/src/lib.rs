//! drove-notify: desktop notifications for drove agents.
//!
//! * [`decide`] - pure policy: state change -> [`decide::Action`]s
//! * [`app`] - per-agent state, rate limiting, action handling
//! * [`backend`] - D-Bus and dry-run outputs

pub mod app;
pub mod backend;
pub mod config;
pub mod decide;
