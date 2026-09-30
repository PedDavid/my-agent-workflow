//! Pure rendering helpers for `ls`, `pick` and `bar`.

use serde_json::{Value, json};

use crate::model::{Agent, Status};

pub fn icon(a: &Agent) -> &'static str {
    match a.status {
        Status::Starting => "◌",
        Status::Idle if a.attention => "●",
        Status::Idle => "○",
        Status::Working => "◐",
        Status::NeedsInput => "◉",
        Status::Exited => "✕",
    }
}

pub fn status_label(a: &Agent) -> String {
    let mut s = a.status.as_str().replace('_', " ");
    if a.maybe {
        s.push('?');
    }
    if a.attention {
        s.push_str(" !");
    }
    s
}

fn home_short(p: &str) -> String {
    let home = crate::paths::home();
    let home = home.to_string_lossy();
    match p.strip_prefix(home.as_ref()) {
        Some(rest) if home != "/" && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => p.to_string(),
    }
}

fn clip(s: &str, max: usize) -> String {
    crate::model::snippet(s, max)
}

/// `drove ls` table.
pub fn table(agents: &[Agent]) -> String {
    let rows: Vec<[String; 8]> = agents
        .iter()
        .map(|a| {
            [
                icon(a).to_string(),
                a.name.clone(),
                a.id.clone(),
                a.kind.as_str().to_string(),
                status_label(a),
                clip(&a.detail, 40),
                a.window
                    .as_ref()
                    .map(|w| w.workspace.clone())
                    .unwrap_or_else(|| "-".into()),
                home_short(&a.cwd),
            ]
        })
        .collect();
    let header = ["", "NAME", "ID", "KIND", "STATUS", "DETAIL", "WS", "CWD"];
    let mut widths = header.map(|h| h.chars().count());
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let fmt = |cells: &[String]| {
        let mut line = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                line.push_str(c);
            } else {
                line.push_str(c);
                let pad = widths[i] - c.chars().count() + 2;
                line.push_str(&" ".repeat(pad));
            }
        }
        line.trim_end().to_string() + "\n"
    };
    let mut out = fmt(&header.map(String::from));
    for r in &rows {
        out.push_str(&fmt(r));
    }
    out
}

/// One picker line: `"<icon> <name>  <status>  <detail>  [<id>]"`.
pub fn pick_line(a: &Agent) -> String {
    let detail = clip(&a.detail, 60);
    let mut s = format!("{} {}  {}", icon(a), a.name, status_label(a));
    if !detail.is_empty() {
        s.push_str("  ");
        s.push_str(&detail);
    }
    s.push_str(&format!("  [{}]", a.id));
    s
}

/// Extract the id from a selected picker line.
pub fn parse_pick(line: &str) -> Option<String> {
    let line = line.trim_end();
    let inner = line.strip_suffix(']')?;
    let start = inner.rfind('[')?;
    let id = &inner[start + 1..];
    (!id.is_empty() && !id.contains(char::is_whitespace)).then(|| id.to_string())
}

/// Picker order: needs input, attention, working, idle, starting, exited.
pub fn pick_order(agents: &mut [Agent]) {
    let rank = |a: &Agent| match (a.status, a.attention) {
        (Status::NeedsInput, _) => 0,
        (Status::Exited, _) => 5,
        (_, true) => 1,
        (Status::Working, _) => 2,
        (Status::Idle, _) => 3,
        (Status::Starting, _) => 4,
    };
    agents.sort_by_key(|a| (rank(a), a.created_at));
}

/// Waybar custom-module JSON for the current agents.
pub fn bar(agents: &[Agent]) -> Value {
    let live: Vec<&Agent> = agents
        .iter()
        .filter(|a| a.status != Status::Exited)
        .collect();
    let count = |f: &dyn Fn(&Agent) -> bool| live.iter().filter(|a| f(a)).count();
    let needs = count(&|a| a.status == Status::NeedsInput);
    let attn = count(&|a| a.status != Status::NeedsInput && a.attention);
    let working = count(&|a| a.status == Status::Working && !a.attention);
    let idle = count(&|a| matches!(a.status, Status::Idle | Status::Starting) && !a.attention);
    let mut parts = vec![];
    if needs > 0 {
        parts.push(format!("◉ {needs}"));
    }
    if attn > 0 {
        parts.push(format!("● {attn}"));
    }
    if working > 0 {
        parts.push(format!("◐ {working}"));
    }
    if idle > 0 {
        parts.push(format!("○ {idle}"));
    }
    let class = if needs > 0 || attn > 0 {
        "needs-input"
    } else if working > 0 {
        "working"
    } else if idle > 0 {
        "idle"
    } else {
        "none"
    };
    let tooltip: Vec<String> = live
        .iter()
        .map(|a| {
            let mut s = format!("{} {} — {}", icon(a), a.name, status_label(a));
            if !a.detail.is_empty() {
                s.push_str(&format!(": {}", clip(&a.detail, 60)));
            }
            s
        })
        .collect();
    json!({
        "text": parts.join("  "),
        "tooltip": if tooltip.is_empty() { "no agents".to_string() } else { tooltip.join("\n") },
        "class": [class],
        "alt": class,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentKind;

    fn agent(id: &str, name: &str, status: Status, attention: bool, created: u64) -> Agent {
        let mut a = Agent::new(id, name, AgentKind::Claude, "claude", "/w", created);
        a.status = status;
        a.attention = attention;
        a
    }

    #[test]
    fn pick_lines_roundtrip() {
        let mut a = agent("abcdef", "api [x]", Status::NeedsInput, true, 0);
        a.detail = "permission: Bash".into();
        let l = pick_line(&a);
        assert_eq!(l, "◉ api [x]  needs input !  permission: Bash  [abcdef]");
        assert_eq!(parse_pick(&l).as_deref(), Some("abcdef"));
        assert_eq!(parse_pick(&format!("{l}\n")).as_deref(), Some("abcdef"));
        assert_eq!(parse_pick("nothing"), None);
        assert_eq!(parse_pick("x []"), None);
    }

    #[test]
    fn ordering() {
        let mut v = vec![
            agent("aaaaaa", "a", Status::Exited, false, 0),
            agent("bbbbbb", "b", Status::Idle, false, 1),
            agent("cccccc", "c", Status::Working, false, 2),
            agent("dddddd", "d", Status::Idle, true, 3),
            agent("eeeeee", "e", Status::NeedsInput, true, 4),
        ];
        pick_order(&mut v);
        let ids: String = v.iter().map(|a| &a.name[..]).collect();
        assert_eq!(ids, "edcba");
    }

    #[test]
    fn bar_json() {
        assert_eq!(bar(&[])["class"], json!(["none"]));
        let v = vec![
            agent("aaaaaa", "a", Status::Working, false, 0),
            agent("bbbbbb", "b", Status::Idle, false, 0),
            agent("cccccc", "c", Status::Exited, false, 0),
        ];
        let b = bar(&v);
        assert_eq!(b["text"], "◐ 1  ○ 1");
        assert_eq!(b["class"], json!(["working"]));
        let v = vec![
            agent("aaaaaa", "a", Status::NeedsInput, true, 0),
            agent("bbbbbb", "b", Status::Idle, true, 0),
        ];
        let b = bar(&v);
        assert_eq!(b["text"], "◉ 1  ● 1");
        assert_eq!(b["class"], json!(["needs-input"]));
        assert!(
            b["tooltip"]
                .as_str()
                .unwrap()
                .contains("◉ a — needs input !")
        );
    }

    #[test]
    fn table_aligns() {
        let t = table(&[agent("abcdef", "api", Status::Idle, true, 0)]);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("   NAME"));
        assert!(lines[1].starts_with("●  api   abcdef  claude  idle !"));
    }
}
