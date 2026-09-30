//! Pure application state and key handling. No I/O happens here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use drove_client::{apply, sort_agents, Agent, Event, Status};

/// Something the UI wants done against the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Focus(String),
    FocusNext,
    Send { id: String, text: String },
    Rename { id: String, name: String },
    Close(String),
    ForgetExited,
    Quit,
}

/// Messages flowing from worker threads into the UI loop.
#[derive(Debug, Clone)]
pub enum Msg {
    Event(Event),
    Connected,
    Disconnected(String),
    /// Result of an executed action: `Ok(info)` or `Err(daemon error)`.
    ActionDone(Result<String, String>),
    Preview {
        id: String,
        text: String,
    },
    PreviewError {
        id: String,
        error: String,
    },
    Tick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Send,
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Input {
        kind: InputKind,
        id: String,
        buf: String,
    },
    ConfirmClose {
        id: String,
        name: String,
    },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    pub text: String,
    pub is_error: bool,
}

pub struct App {
    /// Always kept sorted with `sort_agents`.
    pub agents: Vec<Agent>,
    pub selected_id: Option<String>,
    pub connected: bool,
    pub last_error: Option<String>,
    pub mode: Mode,
    pub status: Option<StatusLine>,
    pub preview: Option<(String, String)>,
    pub tick: usize,
    pub should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        App {
            agents: vec![],
            selected_id: None,
            connected: false,
            last_error: None,
            mode: Mode::Normal,
            status: None,
            preview: None,
            tick: 0,
            should_quit: false,
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected_id.as_ref()?;
        self.agents.iter().position(|a| &a.id == id)
    }

    pub fn selected(&self) -> Option<&Agent> {
        self.selected_index().map(|i| &self.agents[i])
    }

    /// Count of agents per status: starting, idle, working, needs_input, exited.
    pub fn counts(&self) -> [usize; 5] {
        let mut c = [0; 5];
        for a in &self.agents {
            let i = match a.status {
                Status::Starting => 0,
                Status::Idle => 1,
                Status::Working => 2,
                Status::NeedsInput => 3,
                Status::Exited => 4,
            };
            c[i] += 1;
        }
        c
    }

    fn set_status(&mut self, text: impl Into<String>, is_error: bool) {
        self.status = Some(StatusLine {
            text: text.into(),
            is_error,
        });
    }

    /// Re-sort, keeping the selection on the same agent id when possible.
    fn resort(&mut self, old_index: Option<usize>) {
        sort_agents(&mut self.agents);
        if self.selected_index().is_none() {
            self.selected_id = if self.agents.is_empty() {
                None
            } else {
                let i = old_index.unwrap_or(0).min(self.agents.len() - 1);
                Some(self.agents[i].id.clone())
            };
        }
    }

    pub fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Event(ev) => {
                let old = self.selected_index();
                apply(&mut self.agents, ev);
                self.resort(old);
            }
            Msg::Connected => {
                self.connected = true;
                self.last_error = None;
            }
            Msg::Disconnected(e) => {
                self.connected = false;
                self.last_error = Some(e);
            }
            Msg::ActionDone(Ok(s)) => {
                if !s.is_empty() {
                    self.set_status(s, false)
                }
            }
            Msg::ActionDone(Err(e)) => self.set_status(e, true),
            Msg::Preview { id, text } => self.preview = Some((id, text)),
            Msg::PreviewError { id, error } => {
                self.preview = Some((id, format!("(preview unavailable: {error})")))
            }
            Msg::Tick => self.tick = self.tick.wrapping_add(1),
        }
    }

    fn move_sel(&mut self, delta: isize) {
        if self.agents.is_empty() {
            return;
        }
        let cur = self.selected_index().unwrap_or(0) as isize;
        let n = self.agents.len() as isize;
        let i = (cur + delta).clamp(0, n - 1) as usize;
        self.selected_id = Some(self.agents[i].id.clone());
    }

    /// Handle a key press, returning the daemon actions it triggers.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return vec![Action::Quit];
        }
        match self.mode.clone() {
            Mode::Normal => self.key_normal(key),
            Mode::Help => {
                self.mode = Mode::Normal;
                vec![]
            }
            Mode::ConfirmClose { id, .. } => {
                self.mode = Mode::Normal;
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => vec![Action::Close(id)],
                    _ => {
                        self.set_status("close cancelled", false);
                        vec![]
                    }
                }
            }
            Mode::Input { kind, id, mut buf } => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    vec![]
                }
                KeyCode::Enter => {
                    self.mode = Mode::Normal;
                    let text = buf.trim_end().to_string();
                    if text.trim().is_empty() {
                        return vec![];
                    }
                    match kind {
                        InputKind::Send => vec![Action::Send { id, text }],
                        InputKind::Rename => vec![Action::Rename {
                            id,
                            name: text.trim().to_string(),
                        }],
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                    self.mode = Mode::Input { kind, id, buf };
                    vec![]
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buf.push(c);
                    self.mode = Mode::Input { kind, id, buf };
                    vec![]
                }
                _ => vec![],
            },
        }
    }

    fn key_normal(&mut self, key: KeyEvent) -> Vec<Action> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
                vec![Action::Quit]
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_sel(1);
                vec![]
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_sel(-1);
                vec![]
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.move_sel(isize::MIN / 2);
                vec![]
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.move_sel(isize::MAX / 2);
                vec![]
            }
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                vec![]
            }
            KeyCode::Char('n') => vec![Action::FocusNext],
            KeyCode::Char('f') => vec![Action::ForgetExited],
            KeyCode::Enter => self
                .selected()
                .map(|a| vec![Action::Focus(a.id.clone())])
                .unwrap_or_default(),
            KeyCode::Char('s') => {
                if let Some(a) = self.selected() {
                    self.mode = Mode::Input {
                        kind: InputKind::Send,
                        id: a.id.clone(),
                        buf: String::new(),
                    };
                }
                vec![]
            }
            KeyCode::Char('r') => {
                if let Some(a) = self.selected() {
                    self.mode = Mode::Input {
                        kind: InputKind::Rename,
                        id: a.id.clone(),
                        buf: a.name.clone(),
                    };
                }
                vec![]
            }
            KeyCode::Char('x') => {
                if let Some(a) = self.selected() {
                    self.mode = Mode::ConfirmClose {
                        id: a.id.clone(),
                        name: a.name.clone(),
                    };
                }
                vec![]
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(id: &str, status: &str, attention: bool, updated: u64) -> Agent {
        serde_json::from_value(json!({
            "id": id, "name": format!("name-{id}"), "kind": "claude",
            "status": status, "attention": attention, "updated_at": updated
        }))
        .unwrap()
    }

    fn app3() -> App {
        let mut app = App::new();
        app.handle_msg(Msg::Event(Event::Snapshot {
            agents: vec![
                agent("w", "working", false, 5),
                agent("n", "needs_input", false, 1),
                agent("e", "exited", false, 9),
            ],
        }));
        app
    }

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }
    fn ch(c: char) -> KeyEvent {
        k(KeyCode::Char(c))
    }

    #[test]
    fn snapshot_sorts_and_selects_first() {
        let app = app3();
        let ids: Vec<_> = app.agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["n", "w", "e"]);
        assert_eq!(app.selected_id.as_deref(), Some("n"));
        assert_eq!(app.counts(), [0, 0, 1, 1, 1]);
    }

    #[test]
    fn enter_focuses_selected() {
        let mut app = app3();
        app.handle_key(ch('j'));
        assert_eq!(
            app.handle_key(k(KeyCode::Enter)),
            vec![Action::Focus("w".into())]
        );
    }

    #[test]
    fn enter_on_empty_list_does_nothing() {
        let mut app = App::new();
        assert!(app.handle_key(k(KeyCode::Enter)).is_empty());
        assert!(app.handle_key(ch('x')).is_empty());
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn movement_clamps() {
        let mut app = app3();
        app.handle_key(k(KeyCode::Up));
        assert_eq!(app.selected_id.as_deref(), Some("n"));
        for _ in 0..10 {
            app.handle_key(k(KeyCode::Down));
        }
        assert_eq!(app.selected_id.as_deref(), Some("e"));
        app.handle_key(ch('g'));
        assert_eq!(app.selected_id.as_deref(), Some("n"));
        app.handle_key(ch('G'));
        assert_eq!(app.selected_id.as_deref(), Some("e"));
    }

    #[test]
    fn selection_follows_id_across_resort() {
        let mut app = app3();
        app.handle_key(ch('j')); // w
        app.handle_msg(Msg::Event(Event::Agent {
            agent: Box::new(agent("e", "needs_input", true, 100)),
        }));
        assert_eq!(app.agents[0].id, "e");
        assert_eq!(app.selected_id.as_deref(), Some("w"));
    }

    #[test]
    fn removed_selection_falls_back_to_neighbour() {
        let mut app = app3();
        app.handle_key(ch('j'));
        app.handle_msg(Msg::Event(Event::Removed { id: "w".into() }));
        assert_eq!(app.selected_id.as_deref(), Some("e"));
        app.handle_msg(Msg::Event(Event::Snapshot { agents: vec![] }));
        assert_eq!(app.selected_id, None);
    }

    #[test]
    fn next_forget_quit() {
        let mut app = app3();
        assert_eq!(app.handle_key(ch('n')), vec![Action::FocusNext]);
        assert_eq!(app.handle_key(ch('f')), vec![Action::ForgetExited]);
        assert_eq!(app.handle_key(ch('q')), vec![Action::Quit]);
        assert!(app.should_quit);
    }

    #[test]
    fn send_flow() {
        let mut app = app3();
        assert!(app.handle_key(ch('s')).is_empty());
        for c in "hi there".chars() {
            app.handle_key(ch(c));
        }
        app.handle_key(k(KeyCode::Backspace));
        assert_eq!(
            app.handle_key(k(KeyCode::Enter)),
            vec![Action::Send {
                id: "n".into(),
                text: "hi ther".into()
            }]
        );
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn input_esc_cancels_and_empty_sends_nothing() {
        let mut app = app3();
        app.handle_key(ch('s'));
        app.handle_key(ch('a'));
        assert!(app.handle_key(k(KeyCode::Esc)).is_empty());
        assert_eq!(app.mode, Mode::Normal);
        app.handle_key(ch('s'));
        assert!(app.handle_key(k(KeyCode::Enter)).is_empty());
    }

    #[test]
    fn rename_prefilled() {
        let mut app = app3();
        app.handle_key(ch('r'));
        app.handle_key(k(KeyCode::Backspace));
        app.handle_key(ch('Z'));
        assert_eq!(
            app.handle_key(k(KeyCode::Enter)),
            vec![Action::Rename {
                id: "n".into(),
                name: "name-Z".into()
            }]
        );
    }

    #[test]
    fn close_needs_confirmation() {
        let mut app = app3();
        app.handle_key(ch('x'));
        assert!(matches!(app.mode, Mode::ConfirmClose { .. }));
        assert!(app.handle_key(ch('n')).is_empty());
        assert_eq!(app.mode, Mode::Normal);
        app.handle_key(ch('x'));
        assert_eq!(app.handle_key(ch('y')), vec![Action::Close("n".into())]);
    }

    #[test]
    fn help_toggles_and_ctrl_c_quits() {
        let mut app = app3();
        app.handle_key(ch('?'));
        assert_eq!(app.mode, Mode::Help);
        app.handle_key(ch('z'));
        assert_eq!(app.mode, Mode::Normal);
        app.handle_key(ch('s'));
        let a = app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(a, vec![Action::Quit]);
    }

    #[test]
    fn connection_and_errors() {
        let mut app = App::new();
        app.handle_msg(Msg::Disconnected("nope".into()));
        assert!(!app.connected);
        app.handle_msg(Msg::Connected);
        assert!(app.connected && app.last_error.is_none());
        app.handle_msg(Msg::ActionDone(Err("no agent".into())));
        assert_eq!(app.status.as_ref().unwrap().text, "no agent");
        assert!(app.status.as_ref().unwrap().is_error);
    }
}
