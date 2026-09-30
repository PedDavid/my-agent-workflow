//! Hyprland: requests through the `hyprctl` binary, events from socket2.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use super::{Client, WmEvent, norm_addr};
use crate::config::{DispatchMode, HyprlandConfig};
use crate::lua;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Lua,
    Legacy,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Lua => "lua",
            Mode::Legacy => "legacy",
        }
    }
}

pub struct Hyprland {
    hyprctl: String,
    configured: DispatchMode,
    probed: Mutex<Option<Mode>>,
}

impl Hyprland {
    pub fn new(cfg: &HyprlandConfig) -> Self {
        Hyprland {
            hyprctl: cfg.hyprctl.clone(),
            configured: cfg.dispatch,
            probed: Mutex::new(None),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = Command::new(&self.hyprctl)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("running {}", self.hyprctl))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            bail!(
                "{} {} failed: {}{}",
                self.hyprctl,
                args.join(" "),
                stdout.trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(stdout)
    }

    /// Whether the running Hyprland evaluates dispatch arguments as Lua.
    pub fn probe_lua(&self) -> Result<bool> {
        let out = self.run(&["dispatch", lua::NOOP_EXPR])?;
        Ok(out.trim() == "ok")
    }

    /// The dispatch syntax to use (probes once in `auto` mode).
    pub fn mode(&self) -> Mode {
        match self.configured {
            DispatchMode::Lua => return Mode::Lua,
            DispatchMode::Legacy => return Mode::Legacy,
            DispatchMode::Auto => {}
        }
        let mut probed = self.probed.lock().unwrap();
        if let Some(m) = *probed {
            return m;
        }
        match self.probe_lua() {
            Ok(lua) => {
                let m = if lua { Mode::Lua } else { Mode::Legacy };
                *probed = Some(m);
                m
            }
            // hyprctl unreachable: guess, but probe again next time.
            Err(_) => Mode::Lua,
        }
    }

    pub fn clients(&self) -> Result<Vec<Client>> {
        parse_clients(&self.run(&["-j", "clients"])?)
    }

    fn dispatch(&self, args: &[&str]) -> Result<()> {
        let mut full = vec!["dispatch"];
        full.extend_from_slice(args);
        let out = self.run(&full)?;
        if out.trim() != "ok" {
            bail!("hyprctl dispatch {}: {}", args.join(" "), out.trim());
        }
        Ok(())
    }

    pub fn focus(&self, address: &str) -> Result<()> {
        match self.mode() {
            Mode::Lua => self.dispatch(&[&lua::focus_expr(address)]),
            Mode::Legacy => self.dispatch(&["focuswindow", &format!("address:{address}")]),
        }
    }

    pub fn close(&self, address: &str) -> Result<()> {
        match self.mode() {
            Mode::Lua => self.dispatch(&[&lua::close_expr(address)]).or_else(|_| {
                // Older Lua API: close() without a selector acts on the focused window.
                self.focus(address)?;
                self.dispatch(&["hl.dsp.window.close()"])
            }),
            Mode::Legacy => self.dispatch(&["closewindow", &format!("address:{address}")]),
        }
    }

    /// Launch a shell command line through the compositor, applying a workspace rule.
    pub fn exec(&self, cmdline: &str, workspace: &str) -> Result<()> {
        match self.mode() {
            Mode::Lua => self.dispatch(&[&lua::exec_expr(cmdline, workspace)]),
            Mode::Legacy => {
                let arg = if workspace.is_empty() {
                    cmdline.to_string()
                } else {
                    format!("[workspace {workspace}] {cmdline}")
                };
                self.dispatch(&["exec", &arg])
            }
        }
    }
}

#[derive(Deserialize)]
struct RawWs {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawClient {
    address: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    initial_class: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    pid: i64,
    workspace: Option<RawWs>,
    #[serde(default, rename = "focusHistoryID")]
    focus_history_id: i64,
}

pub fn parse_clients(json: &str) -> Result<Vec<Client>> {
    let raw: Vec<RawClient> = serde_json::from_str(json).context("parsing hyprctl -j clients")?;
    Ok(raw
        .into_iter()
        .map(|r| Client {
            address: norm_addr(&r.address),
            class: r.class,
            initial_class: r.initial_class,
            title: r.title,
            pid: r.pid,
            workspace: r.workspace.map(|w| w.name).unwrap_or_default(),
            focus_history_id: r.focus_history_id,
        })
        .collect())
}

/// Parse one socket2 line (`EVENT>>DATA`).
pub fn parse_event(line: &str) -> Option<WmEvent> {
    let (name, data) = line.trim_end_matches(['\r', '\n']).split_once(">>")?;
    let addr = |s: &str| norm_addr(s);
    Some(match name {
        "openwindow" => {
            let mut p = data.splitn(4, ',');
            let address = addr(p.next()?);
            let workspace = p.next()?.to_string();
            let class = p.next()?.to_string();
            let title = p.next().unwrap_or("").to_string();
            WmEvent::OpenWindow {
                address,
                workspace,
                class,
                title,
            }
        }
        "closewindow" => WmEvent::CloseWindow {
            address: addr(data),
        },
        "activewindowv2" => WmEvent::ActiveWindow {
            address: (!data.trim().is_empty() && data.trim() != ",").then(|| addr(data)),
        },
        "windowtitlev2" => {
            let (a, t) = data.split_once(',')?;
            WmEvent::Title {
                address: addr(a),
                title: t.to_string(),
            }
        }
        "movewindowv2" => {
            let mut p = data.splitn(3, ',');
            let address = addr(p.next()?);
            let _id = p.next()?;
            WmEvent::MoveWindow {
                address,
                workspace: p.next()?.to_string(),
            }
        }
        "urgent" => WmEvent::Urgent {
            address: addr(data),
        },
        _ => return None,
    })
}

/// Read socket2 forever, reconnecting with backoff. Sends `None` on each
/// (re)connect so the consumer can resynchronise.
pub async fn event_stream(path: PathBuf, tx: mpsc::Sender<Option<WmEvent>>) {
    let mut backoff = Duration::from_millis(250);
    loop {
        match tokio::net::UnixStream::connect(&path).await {
            Ok(stream) => {
                backoff = Duration::from_millis(250);
                tracing::info!("connected to {}", path.display());
                if tx.send(None).await.is_err() {
                    return;
                }
                let mut lines = tokio::io::BufReader::new(stream).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if let Some(ev) = parse_event(&line)
                                && tx.send(Some(ev)).await.is_err()
                            {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!("socket2 read: {e}");
                            break;
                        }
                    }
                }
                tracing::warn!("socket2 disconnected");
            }
            Err(e) => tracing::debug!("socket2 connect {}: {e}", path.display()),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events() {
        assert_eq!(
            parse_event("openwindow>>55d1c0ffee,3,drove-abcdef,claude: fix, things"),
            Some(WmEvent::OpenWindow {
                address: "0x55d1c0ffee".into(),
                workspace: "3".into(),
                class: "drove-abcdef".into(),
                title: "claude: fix, things".into(),
            })
        );
        assert_eq!(
            parse_event("closewindow>>55d1"),
            Some(WmEvent::CloseWindow {
                address: "0x55d1".into()
            })
        );
        assert_eq!(
            parse_event("activewindowv2>>55d1"),
            Some(WmEvent::ActiveWindow {
                address: Some("0x55d1".into())
            })
        );
        assert_eq!(
            parse_event("activewindowv2>>"),
            Some(WmEvent::ActiveWindow { address: None })
        );
        assert_eq!(
            parse_event("windowtitlev2>>55d1,a, b"),
            Some(WmEvent::Title {
                address: "0x55d1".into(),
                title: "a, b".into()
            })
        );
        assert_eq!(
            parse_event("movewindowv2>>55d1,-98,special:agents"),
            Some(WmEvent::MoveWindow {
                address: "0x55d1".into(),
                workspace: "special:agents".into()
            })
        );
        assert_eq!(
            parse_event("urgent>>55d1\n"),
            Some(WmEvent::Urgent {
                address: "0x55d1".into()
            })
        );
        assert_eq!(parse_event("workspace>>3"), None);
        assert_eq!(parse_event("garbage"), None);
        assert_eq!(parse_event("openwindow>>55d1"), None);
    }

    #[test]
    fn clients() {
        let json = r#"[
          {"address":"0x55d1c0ffee","mapped":true,"hidden":false,"at":[0,0],"size":[10,10],
           "workspace":{"id":3,"name":"3"},"floating":false,"monitor":0,
           "class":"drove-abcdef","title":"claude","initialClass":"drove-abcdef",
           "initialTitle":"claude","pid":4242,"xwayland":false,"pinned":false,
           "fullscreen":0,"tags":[],"focusHistoryID":1},
          {"address":"0x1","class":"firefox","pid":1,"workspace":{"id":1,"name":"1"}}
        ]"#;
        let c = parse_clients(json).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].address, "0x55d1c0ffee");
        assert_eq!(c[0].class, "drove-abcdef");
        assert_eq!(c[0].initial_class, "drove-abcdef");
        assert_eq!(c[0].pid, 4242);
        assert_eq!(c[0].workspace, "3");
        assert_eq!(c[0].focus_history_id, 1);
        assert!(parse_clients("nope").is_err());
    }

    fn fake(dir: &std::path::Path, reply: &str) -> HyprlandConfig {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("hyprctl");
        let log = dir.join("log");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {log}; done\necho --- >> {log}\necho '{reply}'\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        HyprlandConfig {
            hyprctl: bin.to_string_lossy().into(),
            ..Default::default()
        }
    }

    #[test]
    fn dispatch_modes() {
        let dir = tempfile::tempdir().unwrap();
        let h = Hyprland::new(&fake(dir.path(), "ok"));
        assert_eq!(h.mode(), Mode::Lua);
        h.focus("0x1").unwrap();
        h.exec("kitty --class drove-abcdef", "3 silent").unwrap();
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        assert!(log.contains("dispatch\nhl.dsp.no_op()\n---"));
        assert!(log.contains("dispatch\nhl.dsp.focus({ window = \"address:0x1\" })\n---"));
        assert!(log.contains(
            "dispatch\nhl.dsp.exec_cmd(\"kitty --class drove-abcdef\", { workspace = \"3 silent\" })\n"
        ));

        let dir = tempfile::tempdir().unwrap();
        let h = Hyprland::new(&fake(dir.path(), "Invalid dispatcher"));
        assert_eq!(h.mode(), Mode::Legacy);
        assert!(h.focus("0x1").is_err(), "non-ok reply is an error");
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        assert!(log.contains("dispatch\nfocuswindow\naddress:0x1\n---"));

        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fake(dir.path(), "ok");
        cfg.dispatch = DispatchMode::Legacy;
        let h = Hyprland::new(&cfg);
        h.exec("kitty x", "3 silent").unwrap();
        h.close("0x2").unwrap();
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        assert!(!log.contains("no_op"));
        assert!(log.contains("dispatch\nexec\n[workspace 3 silent] kitty x\n---"));
        assert!(log.contains("dispatch\nclosewindow\naddress:0x2\n---"));
    }
}
