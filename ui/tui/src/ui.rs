//! Rendering. Reads an [`App`], draws into a ratatui frame; no state changes.

use drove_client::{Agent, Status};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, InputKind, Mode};

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

pub fn glyph(status: Status, tick: usize) -> (&'static str, Style) {
    match status {
        Status::Starting => ("○", Style::default().fg(Color::Gray)),
        Status::Idle => ("●", Style::default().fg(Color::Green)),
        Status::Working => (
            SPINNER[tick % SPINNER.len()],
            Style::default().fg(Color::Cyan),
        ),
        Status::NeedsInput => (
            "!",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Status::Exited => ("×", Style::default().fg(Color::DarkGray)),
    }
}

/// `/home/me/src/api` -> `~/src/api` (when $HOME matches), long paths -> `…/last/two`.
pub fn short_cwd(cwd: &str, home: Option<&str>) -> String {
    let mut p = cwd.to_string();
    if let Some(h) = home.filter(|h| !h.is_empty() && *h != "/") {
        if p == h {
            p = "~".into();
        } else if let Some(rest) = p.strip_prefix(&format!("{h}/")) {
            p = format!("~/{rest}");
        }
    }
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() > 3 {
        format!("…/{}", parts[parts.len() - 2..].join("/"))
    } else {
        p
    }
}

fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n >= w {
        s.chars().take(w).collect()
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

fn row(a: &Agent, tick: usize, home: Option<&str>) -> ListItem<'static> {
    let dim = a.status == Status::Exited;
    let base = if dim {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    let (g, gs) = glyph(a.status, tick);
    let ws = a
        .window
        .as_ref()
        .map(|w| format!("ws{}", w.workspace))
        .unwrap_or_else(|| "-".into());
    let mut spans = vec![
        if a.attention {
            Span::styled(
                "▶",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw(" ")
        },
        Span::raw(" "),
        Span::styled(g, gs),
        Span::raw(" "),
        Span::styled(
            pad(&a.name, 18),
            base.add_modifier(if a.attention {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
        Span::styled(pad(&a.kind, 7), base.fg(Color::Blue)),
        Span::styled(pad(&ws, 5), base),
        Span::styled(pad(&short_cwd(&a.cwd, home), 24), base.fg(Color::DarkGray)),
        Span::styled(a.detail.clone(), base),
    ];
    if a.maybe {
        spans.push(Span::styled(" (guess)", Style::default().fg(Color::Yellow)));
    }
    if a.adopted {
        spans.push(Span::styled(
            " adopted",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn header(app: &App) -> Line<'static> {
    let c = app.counts();
    let mut spans = vec![Span::styled(
        " drove ",
        Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
    )];
    let items = [
        (Status::NeedsInput, c[3], "needs input"),
        (Status::Working, c[2], "working"),
        (Status::Idle, c[1], "idle"),
        (Status::Starting, c[0], "starting"),
        (Status::Exited, c[4], "exited"),
    ];
    for (st, n, label) in items {
        let (g, s) = glyph(st, app.tick);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(g, s));
        spans.push(Span::raw(format!(" {n} {label}")));
    }
    Line::from(spans)
}

pub fn help_lines() -> Vec<&'static str> {
    vec![
        "j/k, ↑/↓   move selection",
        "g / G      first / last",
        "Enter      focus the agent's window",
        "n          focus next agent needing you",
        "s          send a prompt (Enter sends, Esc cancels)",
        "r          rename",
        "x          close window (asks y/n)",
        "f          forget all exited agents",
        "?          this help",
        "q, Ctrl-C  quit",
    ]
}

pub fn draw(f: &mut Frame, app: &App) {
    let home = std::env::var("HOME").ok();
    draw_with_home(f, app, home.as_deref());
}

pub fn draw_with_home(f: &mut Frame, app: &App, home: Option<&str>) {
    let banner_h = if app.connected { 0 } else { 1 };
    let [head, banner, list_area, preview_area, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(banner_h),
        Constraint::Percentage(45),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(f.area());

    f.render_widget(Paragraph::new(header(app)), head);

    if !app.connected {
        let why = app.last_error.clone().unwrap_or_default();
        f.render_widget(
            Paragraph::new(format!(" daemon unreachable, reconnecting… {why}")).style(
                Style::default()
                    .bg(Color::Red)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            banner,
        );
    }

    let items: Vec<ListItem> = app.agents.iter().map(|a| row(a, app.tick, home)).collect();
    let mut state = ListState::default().with_selected(app.selected_index());
    let empty = items.is_empty();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::BOTTOM)
                .title(" agents "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, list_area, &mut state);
    if empty {
        let inner = Rect {
            x: list_area.x + 1,
            y: list_area.y + 1,
            width: list_area.width.saturating_sub(2),
            height: 1,
        };
        f.render_widget(
            Paragraph::new("no agents").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
    }

    draw_preview(f, app, preview_area);
    draw_footer(f, app, footer);

    if app.mode == Mode::Help {
        let lines = help_lines();
        let w = 56.min(f.area().width);
        let h = (lines.len() as u16 + 2).min(f.area().height);
        let area = Rect {
            x: f.area().width.saturating_sub(w) / 2,
            y: f.area().height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
                .block(Block::default().borders(Borders::ALL).title(" help ")),
            area,
        );
    }
}

fn draw_preview(f: &mut Frame, app: &App, area: Rect) {
    let title = match app.selected() {
        Some(a) => format!(" {} · {} ", a.name, a.id),
        None => " preview ".to_string(),
    };
    let body = match (app.selected(), &app.preview) {
        (Some(a), Some((id, text))) if *id == a.id => {
            // Show the tail of the screen if it does not fit.
            let lines: Vec<&str> = text.trim_end().lines().collect();
            let h = area.height.saturating_sub(2) as usize;
            lines[lines.len().saturating_sub(h)..].join("\n")
        }
        (Some(_), _) => "loading…".to_string(),
        (None, _) => String::new(),
    };
    f.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::TOP).title(title)),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let [status, input] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    if let Some(s) = &app.status {
        let style = if s.is_error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        f.render_widget(Paragraph::new(format!(" {}", s.text)).style(style), status);
    }
    let line = match &app.mode {
        Mode::Input { kind, buf, .. } => {
            let label = match kind {
                InputKind::Send => "send",
                InputKind::Rename => "rename",
            };
            Line::from(vec![
                Span::styled(
                    format!(" {label}> "),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(buf.clone()),
                Span::styled("█", Style::default().fg(Color::Gray)),
            ])
        }
        Mode::ConfirmClose { name, .. } => Line::from(Span::styled(
            format!(" close {name}? (y/n)"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        _ => Line::from(Span::styled(
            " j/k move  Enter focus  n next  s send  r rename  x close  f forget  ? help  q quit",
            Style::default().fg(Color::DarkGray),
        )),
    };
    f.render_widget(Paragraph::new(line), input);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use drove_client::Event;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::json;

    fn agent(v: serde_json::Value) -> Agent {
        serde_json::from_value(v).unwrap()
    }

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw_with_home(f, app, Some("/home/me")))
            .unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample() -> App {
        let mut app = App::new();
        app.handle_msg(Msg::Connected);
        app.handle_msg(Msg::Event(Event::Snapshot {
            agents: vec![
                agent(json!({"id":"aaaaaa","name":"api-refactor","kind":"claude","status":"working",
                    "cwd":"/home/me/src/api","detail":"Bash: cargo test",
                    "window":{"address":"0x1","workspace":"3"}, "updated_at": 1})),
                agent(json!({"id":"bbbbbb","name":"flaky","kind":"codex","status":"needs_input",
                    "attention":true,"maybe":true,"detail":"Allow?","updated_at": 2})),
                agent(json!({"id":"cccccc","name":"docs","kind":"kiro","status":"idle",
                    "adopted":true,"updated_at": 3})),
                agent(json!({"id":"dddddd","name":"old","kind":"claude","status":"exited","updated_at": 4})),
                agent(json!({"id":"eeeeee","name":"fresh","kind":"claude","status":"starting","updated_at": 5})),
            ],
        }));
        app
    }

    #[test]
    fn renders_rows_glyphs_and_counts() {
        let s = render(&sample(), 110, 24);
        for needle in [
            "api-refactor",
            "claude",
            "codex",
            "ws3",
            "~/src/api",
            "Bash: cargo test",
            "(guess)",
            "adopted",
            "!",
            "●",
            "×",
            "○",
            "▶",
            "1 needs input",
            "1 working",
            "1 idle",
            "1 starting",
            "1 exited",
        ] {
            assert!(s.contains(needle), "missing {needle:?} in:\n{s}");
        }
        assert!(!s.contains("unreachable"));
    }

    #[test]
    fn needs_input_listed_before_working() {
        let s = render(&sample(), 110, 24);
        assert!(s.find("flaky").unwrap() < s.find("api-refactor").unwrap());
    }

    #[test]
    fn banner_when_disconnected() {
        let mut app = sample();
        app.handle_msg(Msg::Disconnected("connection refused".into()));
        let s = render(&app, 100, 20);
        assert!(s.contains("daemon unreachable"), "{s}");
        assert!(s.contains("connection refused"));
    }

    #[test]
    fn preview_and_status_line() {
        let mut app = sample();
        let id = app.selected().unwrap().id.clone();
        app.handle_msg(Msg::Preview {
            id,
            text: "hello from the screen\n> prompt".into(),
        });
        app.handle_msg(Msg::ActionDone(Err("no agent matching \"zz\"".into())));
        let s = render(&app, 100, 24);
        assert!(s.contains("hello from the screen"), "{s}");
        assert!(s.contains("no agent matching"));
    }

    #[test]
    fn stale_preview_for_other_agent_not_shown() {
        let mut app = sample();
        app.handle_msg(Msg::Preview {
            id: "zzzzzz".into(),
            text: "WRONG".into(),
        });
        let s = render(&app, 100, 24);
        assert!(!s.contains("WRONG"));
    }

    #[test]
    fn input_confirm_and_help_overlays() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let mut app = sample();
        app.handle_key(key('s'));
        app.handle_key(key('h'));
        app.handle_key(key('i'));
        assert!(render(&app, 100, 24).contains("send> hi"));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(key('x'));
        assert!(render(&app, 100, 24).contains("close flaky? (y/n)"));
        app.handle_key(key('n'));
        app.handle_key(key('?'));
        assert!(render(&app, 100, 24).contains("forget all exited"));
    }

    #[test]
    fn empty_state() {
        let mut app = App::new();
        app.handle_msg(Msg::Connected);
        assert!(render(&app, 80, 16).contains("no agents"));
    }

    #[test]
    fn spinner_animates() {
        assert_ne!(glyph(Status::Working, 0).0, glyph(Status::Working, 1).0);
    }

    #[test]
    fn short_cwd_cases() {
        assert_eq!(short_cwd("/home/me/src/api", Some("/home/me")), "~/src/api");
        assert_eq!(short_cwd("/home/me", Some("/home/me")), "~");
        assert_eq!(short_cwd("/a/b/c/d/e", None), "…/d/e");
        assert_eq!(short_cwd("/tmp/x", None), "/tmp/x");
    }
}
