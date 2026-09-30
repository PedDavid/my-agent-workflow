//! Pure notification policy: which actions does a state change produce?

use std::time::{Duration, Instant};

use drove_client::{Agent, Status};
use serde::Serialize;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    NeedsInput,
    Finished,
    Exited,
}

impl Transition {
    pub fn category(self) -> &'static str {
        match self {
            Transition::NeedsInput => "x-drove.needs-input",
            Transition::Finished => "x-drove.finished",
            Transition::Exited => "x-drove.exited",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

/// One notification to show (replaces any earlier one for the same agent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notice {
    pub agent_id: String,
    pub name: String,
    pub transition: Transition,
    pub summary: String,
    pub body: String,
    pub urgency: Urgency,
    pub category: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Notify(Notice),
    /// Close the agent's notification (if any is open).
    Close {
        agent_id: String,
    },
    /// Run the configured sound hook.
    Sound {
        agent_id: String,
        transition: Transition,
        command: String,
    },
}

/// Everything `decide` needs besides the two agent states.
pub struct Ctx<'a> {
    pub cfg: &'a Config,
    /// Quiet mode (config or runtime file) is active.
    pub quiet: bool,
    pub now: Instant,
    /// When we last notified for this agent.
    pub last_notified: Option<Instant>,
    /// The exit was requested through us; not worth a notification.
    pub expected_exit: bool,
}

impl Ctx<'_> {
    fn rate_limited(&self) -> bool {
        let limit = self.cfg.rate_limit_secs;
        if limit <= 0.0 {
            return false;
        }
        match self.last_notified {
            Some(t) => self.now.saturating_duration_since(t) < Duration::from_secs_f64(limit),
            None => false,
        }
    }
}

fn notice(next: &Agent, t: Transition) -> Notice {
    let (summary, body, urgency) = match t {
        Transition::NeedsInput => {
            let mut body = if next.detail.trim().is_empty() {
                "waiting for your input".to_string()
            } else {
                next.detail.trim().to_string()
            };
            if next.maybe {
                body.push_str(" (guess)");
            }
            (
                format!("⚠ {} needs you", next.name),
                body,
                Urgency::Critical,
            )
        }
        Transition::Finished => (
            format!("✓ {} finished", next.name),
            next.detail.trim().to_string(),
            Urgency::Normal,
        ),
        Transition::Exited => (
            format!("× {} exited", next.name),
            next.detail.trim().to_string(),
            Urgency::Low,
        ),
    };
    Notice {
        agent_id: next.id.clone(),
        name: next.name.clone(),
        transition: t,
        summary,
        body,
        urgency,
        category: t.category().to_string(),
    }
}

/// Decide what to do when an agent changes from `prev` to `next`.
///
/// `prev == None` means we have no baseline (agent just appeared): nothing is
/// notified. Snapshots never go through here for notifications.
pub fn decide(prev: Option<&Agent>, next: &Agent, ctx: &Ctx) -> Vec<Action> {
    let Some(prev) = prev else {
        return vec![];
    };
    let transition = match (prev.status, next.status) {
        (p, Status::NeedsInput) if p != Status::NeedsInput => Some(Transition::NeedsInput),
        (Status::Working, Status::Idle) if next.attention => Some(Transition::Finished),
        (p, Status::Exited) if p != Status::Exited => Some(Transition::Exited),
        _ => None,
    };

    let mut out = Vec::new();
    match transition {
        Some(Transition::Exited) if ctx.expected_exit => out.push(close(next)),
        Some(t) => {
            if ctx.cfg.transitions.enabled(t) && !ctx.quiet && !ctx.rate_limited() {
                out.push(Action::Notify(notice(next, t)));
                if let Some(cmd) = ctx.cfg.sound.command(t) {
                    out.push(Action::Sound {
                        agent_id: next.id.clone(),
                        transition: t,
                        command: cmd.to_string(),
                    });
                }
            }
        }
        None => {
            let started_working = next.status == Status::Working && prev.status != Status::Working;
            let looked_at = prev.attention && !next.attention;
            let resolved = prev.status == Status::NeedsInput && next.status != Status::NeedsInput;
            if started_working || looked_at || resolved {
                out.push(close(next));
            }
        }
    }
    out
}

fn close(a: &Agent) -> Action {
    Action::Close {
        agent_id: a.id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(status: Status, attention: bool) -> Agent {
        serde_json::from_value(json!({
            "id": "ab12cd", "name": "api", "kind": "claude",
            "status": status, "attention": attention, "detail": "some detail",
        }))
        .unwrap()
    }

    fn ctx(cfg: &Config) -> Ctx<'_> {
        Ctx {
            cfg,
            quiet: false,
            now: Instant::now(),
            last_notified: None,
            expected_exit: false,
        }
    }

    fn run(prev: Status, pa: bool, next: Status, na: bool) -> Vec<Action> {
        let cfg = Config::default();
        decide(Some(&agent(prev, pa)), &agent(next, na), &ctx(&cfg))
    }

    fn only_notice(v: Vec<Action>) -> Notice {
        match v.as_slice() {
            [Action::Notify(n)] => n.clone(),
            other => panic!("expected one Notify, got {other:?}"),
        }
    }

    #[test]
    fn working_to_needs_input_is_critical() {
        let n = only_notice(run(Status::Working, false, Status::NeedsInput, true));
        assert_eq!(n.summary, "⚠ api needs you");
        assert_eq!(n.body, "some detail");
        assert_eq!(n.urgency, Urgency::Critical);
        assert_eq!(n.category, "x-drove.needs-input");
    }

    #[test]
    fn any_status_to_needs_input_notifies() {
        for p in [Status::Starting, Status::Idle, Status::Working] {
            only_notice(run(p, false, Status::NeedsInput, true));
        }
    }

    #[test]
    fn needs_input_guess_and_empty_detail() {
        let cfg = Config::default();
        let mut next = agent(Status::NeedsInput, true);
        next.maybe = true;
        let v = decide(Some(&agent(Status::Working, false)), &next, &ctx(&cfg));
        assert_eq!(only_notice(v).body, "some detail (guess)");
        next.detail.clear();
        let v = decide(Some(&agent(Status::Working, false)), &next, &ctx(&cfg));
        assert_eq!(only_notice(v).body, "waiting for your input (guess)");
    }

    #[test]
    fn needs_input_to_needs_input_is_not_repeated() {
        assert!(run(Status::NeedsInput, true, Status::NeedsInput, true).is_empty());
    }

    #[test]
    fn finished_requires_attention() {
        let n = only_notice(run(Status::Working, false, Status::Idle, true));
        assert_eq!(n.summary, "✓ api finished");
        assert_eq!(n.urgency, Urgency::Normal);
        assert_eq!(n.category, "x-drove.finished");
        // user was looking: no notification
        let v = run(Status::Working, false, Status::Idle, false);
        assert!(!v.iter().any(|a| matches!(a, Action::Notify(_))));
    }

    #[test]
    fn only_working_to_idle_counts_as_finished() {
        assert!(run(Status::Starting, false, Status::Idle, true).is_empty());
        assert!(run(Status::Idle, false, Status::Idle, true).is_empty());
        // needs_input answered and agent went idle: not "finished"
        let v = run(Status::NeedsInput, true, Status::Idle, true);
        assert_eq!(
            v,
            vec![Action::Close {
                agent_id: "ab12cd".into()
            }]
        );
    }

    #[test]
    fn exit_is_low_urgency() {
        for p in [Status::Idle, Status::Working, Status::NeedsInput] {
            let n = only_notice(run(p, false, Status::Exited, false));
            assert_eq!(n.summary, "× api exited");
            assert_eq!(n.urgency, Urgency::Low);
            assert_eq!(n.category, "x-drove.exited");
        }
        assert!(run(Status::Exited, false, Status::Exited, false).is_empty());
    }

    #[test]
    fn expected_exit_only_closes() {
        let cfg = Config::default();
        let mut c = ctx(&cfg);
        c.expected_exit = true;
        let v = decide(
            Some(&agent(Status::Idle, false)),
            &agent(Status::Exited, false),
            &c,
        );
        assert_eq!(
            v,
            vec![Action::Close {
                agent_id: "ab12cd".into()
            }]
        );
    }

    #[test]
    fn no_baseline_no_notification() {
        let cfg = Config::default();
        for s in [Status::NeedsInput, Status::Exited, Status::Idle] {
            assert!(decide(None, &agent(s, true), &ctx(&cfg)).is_empty());
        }
    }

    #[test]
    fn closes_when_working_again_or_focused() {
        let close = vec![Action::Close {
            agent_id: "ab12cd".into(),
        }];
        assert_eq!(run(Status::NeedsInput, true, Status::Working, false), close);
        assert_eq!(run(Status::Idle, true, Status::Working, false), close);
        // focused elsewhere: attention cleared while still idle
        assert_eq!(run(Status::Idle, true, Status::Idle, false), close);
        // needs_input while user then looks at it
        assert_eq!(
            run(Status::NeedsInput, true, Status::NeedsInput, false),
            close
        );
        // working -> working (detail changes): nothing
        assert!(run(Status::Working, false, Status::Working, false).is_empty());
    }

    #[test]
    fn rate_limit_suppresses_within_window() {
        let cfg = Config::default(); // 10s
        let now = Instant::now();
        let prev = agent(Status::Working, false);
        let next = agent(Status::NeedsInput, true);
        let mut c = Ctx {
            cfg: &cfg,
            quiet: false,
            now,
            last_notified: Some(now),
            expected_exit: false,
        };
        assert!(decide(Some(&prev), &next, &c).is_empty());
        c.now = now + Duration::from_secs(9);
        assert!(decide(Some(&prev), &next, &c).is_empty());
        c.now = now + Duration::from_secs(10);
        assert_eq!(decide(Some(&prev), &next, &c).len(), 1);
    }

    #[test]
    fn rate_limit_zero_disables() {
        let cfg = Config {
            rate_limit_secs: 0.0,
            ..Config::default()
        };
        let now = Instant::now();
        let c = Ctx {
            cfg: &cfg,
            quiet: false,
            now,
            last_notified: Some(now),
            expected_exit: false,
        };
        let v = decide(
            Some(&agent(Status::Working, false)),
            &agent(Status::NeedsInput, true),
            &c,
        );
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn quiet_suppresses_notify_and_sound_but_not_close() {
        let mut cfg = Config::default();
        cfg.sound.needs_input = "true".into();
        let mut c = ctx(&cfg);
        c.quiet = true;
        let v = decide(
            Some(&agent(Status::Working, false)),
            &agent(Status::NeedsInput, true),
            &c,
        );
        assert!(v.is_empty());
        let v = decide(
            Some(&agent(Status::NeedsInput, true)),
            &agent(Status::Working, false),
            &c,
        );
        assert!(matches!(v.as_slice(), [Action::Close { .. }]));
    }

    #[test]
    fn disabled_transition_is_silent() {
        let mut cfg = Config::default();
        cfg.transitions.finished = false;
        cfg.sound.finished = "true".into();
        let v = decide(
            Some(&agent(Status::Working, false)),
            &agent(Status::Idle, true),
            &ctx(&cfg),
        );
        assert!(v.is_empty());
    }

    #[test]
    fn sound_hook_follows_notify() {
        let mut cfg = Config::default();
        cfg.sound.exited = "pw-play x".into();
        let v = decide(
            Some(&agent(Status::Working, false)),
            &agent(Status::Exited, false),
            &ctx(&cfg),
        );
        assert_eq!(v.len(), 2);
        assert!(matches!(&v[0], Action::Notify(_)));
        assert_eq!(
            v[1],
            Action::Sound {
                agent_id: "ab12cd".into(),
                transition: Transition::Exited,
                command: "pw-play x".into()
            }
        );
    }
}
