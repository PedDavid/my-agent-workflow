//! Command line interface.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::client::{self, Client};
use crate::config::Config;
use crate::hooks_install as hi;
use crate::model::Agent;
use crate::protocol::{Event, SpawnParams};
use crate::ui;

#[derive(Parser)]
#[command(
    name = "drove",
    version,
    about = "Herd coding agents, one Hyprland window each"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum HookSource {
    Claude,
    Codex,
    Kiro,
    CodexNotify,
}

#[derive(Clone, Copy, ValueEnum)]
enum HookTarget {
    Claude,
    Codex,
    Kiro,
}

#[derive(Subcommand)]
enum HooksCmd {
    /// Print the hook configuration snippet for an agent.
    Print { target: HookTarget },
    /// Merge drove's hooks into the agent's config (idempotent, backs up first).
    Install {
        target: HookTarget,
        /// kiro: agent config name (default: every file in ~/.kiro/agents).
        #[arg(long)]
        agent: Option<String>,
        /// Write to this file instead of the default location.
        #[arg(long)]
        file: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon (normally started by Hyprland or systemd).
    Daemon,
    /// Start an agent in a new window; prints its id.
    Spawn {
        /// Profile name (claude, codex, kiro, shell, or one from config.toml).
        profile: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        /// Create/reuse a git worktree for this branch and start there.
        #[arg(long)]
        worktree: Option<String>,
        /// Hyprland workspace rule, e.g. "3 silent" or "special:agents".
        #[arg(long)]
        workspace: Option<String>,
        /// Extra arguments for the agent.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// List agents.
    Ls {
        #[arg(long)]
        json: bool,
        /// Include exited agents.
        #[arg(long, short)]
        all: bool,
    },
    /// Focus an agent's window.
    Focus { agent: String },
    /// Focus the agent that most needs you.
    Next,
    /// Close an agent's window.
    Close { agent: String },
    /// Forget an agent (or all exited ones).
    Forget {
        agent: Option<String>,
        #[arg(long, conflicts_with = "agent")]
        exited: bool,
    },
    /// Rename an agent.
    Rename { agent: String, name: String },
    /// Type text into an agent's terminal.
    Send {
        agent: String,
        /// Press Enter afterwards.
        #[arg(long, short)]
        enter: bool,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Print an agent's terminal contents.
    Text {
        agent: String,
        /// screen | all | last_cmd_output
        #[arg(long, default_value = "screen")]
        extent: String,
    },
    /// Choose an agent with a dmenu-style picker and focus it.
    Pick,
    /// Waybar custom module: JSON lines on every change.
    Bar,
    /// Print the raw event stream.
    Watch,
    /// Hook entry point used by agents (reads the payload on stdin).
    Hook {
        source: HookSource,
        /// JSON payload as an argument (codex-notify).
        payload: Option<String>,
    },
    /// Print or install agent hook configuration.
    Hooks {
        #[command(subcommand)]
        cmd: HooksCmd,
    },
    /// Daemon and compositor status.
    Status,
}

pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    // Fast path: never let clap or config errors reach an agent's hook.
    if args.get(1).map(String::as_str) == Some("hook") {
        let src = args.get(2).map(String::as_str).unwrap_or("");
        return client::hook_main(src, args.get(3).map(String::as_str));
    }
    let cli = Cli::parse();
    match dispatch(cli.cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("drove: {e:#}");
            1
        }
    }
}

fn agents(v: Value) -> Result<Vec<Agent>> {
    Ok(serde_json::from_value(v)?)
}

fn dispatch(cmd: Cmd) -> Result<i32> {
    match cmd {
        Cmd::Daemon => {
            let level = std::env::var("DROVE_LOG")
                .ok()
                .and_then(|l| l.parse().ok())
                .unwrap_or(tracing::Level::INFO);
            tracing_subscriber::fmt()
                .with_max_level(level)
                .with_writer(std::io::stderr)
                .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
                .init();
            crate::daemon::run(Config::load()?)?;
        }
        Cmd::Spawn {
            profile,
            name,
            cwd,
            worktree,
            workspace,
            args,
        } => {
            let cwd = match cwd {
                Some(c) => std::fs::canonicalize(&c)
                    .with_context(|| format!("--cwd {c}"))?
                    .to_string_lossy()
                    .into_owned(),
                None => std::env::current_dir()?.to_string_lossy().into_owned(),
            };
            let p = SpawnParams {
                profile,
                name,
                cwd: Some(cwd),
                worktree,
                workspace,
                args,
            };
            let a: Agent =
                serde_json::from_value(client::call("spawn", serde_json::to_value(p)?)?)?;
            println!("{}", a.id);
        }
        Cmd::Ls { json, all } => {
            let mut list = agents(client::call("list", json!({}))?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                if !all {
                    list.retain(|a| a.status != crate::model::Status::Exited);
                }
                print!("{}", ui::table(&list));
            }
        }
        Cmd::Focus { agent } => {
            client::call("focus", json!({"id": agent}))?;
        }
        Cmd::Next => {
            if client::call("next", json!({}))?.is_null() {
                eprintln!("nothing needs attention");
                return Ok(1);
            }
        }
        Cmd::Close { agent } => {
            client::call("close", json!({"id": agent}))?;
        }
        Cmd::Forget { agent, exited } => {
            if agent.is_none() && !exited {
                bail!("give an agent or --exited");
            }
            let n = client::call("forget", json!({"id": agent, "exited": exited}))?;
            eprintln!("forgot {n}");
        }
        Cmd::Rename { agent, name } => {
            client::call("rename", json!({"id": agent, "name": name}))?;
        }
        Cmd::Send { agent, enter, text } => {
            client::call(
                "send_text",
                json!({"id": agent, "text": text.join(" "), "enter": enter}),
            )?;
        }
        Cmd::Text { agent, extent } => {
            let t = client::call("get_text", json!({"id": agent, "extent": extent}))?;
            print!("{}", t.as_str().unwrap_or(""));
        }
        Cmd::Pick => return pick(),
        Cmd::Bar => bar(),
        Cmd::Watch => {
            let out = std::io::stdout();
            for line in Client::connect_default()?.subscribe_raw()? {
                let mut o = out.lock();
                writeln!(o, "{line}")?;
                o.flush()?;
            }
        }
        Cmd::Hook { .. } => unreachable!("handled before parsing"),
        Cmd::Hooks { cmd } => hooks(cmd)?,
        Cmd::Status => match client::call("status", json!({})) {
            Ok(s) => {
                println!("daemon: running (pid {})", s["pid"]);
                println!("socket: {}", s["socket"].as_str().unwrap_or(""));
                println!("state: {}", s["state"].as_str().unwrap_or(""));
                println!("agents: {}", s["agents"]);
                if s["hyprland"].as_bool().unwrap_or(false) {
                    println!(
                        "hyprland: yes (dispatch: {})",
                        s["dispatch"].as_str().unwrap_or("?")
                    );
                } else {
                    println!("hyprland: not detected");
                }
            }
            Err(e) => {
                println!("daemon: not running ({e:#})");
                return Ok(1);
            }
        },
    }
    Ok(0)
}

fn pick() -> Result<i32> {
    let cfg = Config::load()?;
    let mut list = agents(client::call("list", json!({}))?)?;
    list.retain(|a| a.window.is_some());
    if list.is_empty() {
        eprintln!("no agents with windows");
        return Ok(1);
    }
    ui::pick_order(&mut list);
    let input: String = list.iter().map(|a| ui::pick_line(a) + "\n").collect();
    let (prog, args) = cfg
        .picker
        .command
        .split_first()
        .ok_or_else(|| anyhow!("picker.command is empty"))?;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("running picker {prog}"))?;
    child.stdin.take().unwrap().write_all(input.as_bytes())?;
    let out = child.wait_with_output()?;
    let sel = String::from_utf8_lossy(&out.stdout);
    let Some(id) = sel.lines().next().and_then(ui::parse_pick) else {
        return Ok(1);
    };
    client::call("focus", json!({"id": id}))?;
    Ok(0)
}

fn bar() -> ! {
    use std::collections::BTreeMap;
    let out = std::io::stdout();
    let emit = |v: Value| {
        let mut o = out.lock();
        let _ = writeln!(o, "{v}");
        let _ = o.flush();
    };
    let mut last = Value::Null;
    loop {
        let stream = Client::connect_default().and_then(|c| c.subscribe());
        match stream {
            Ok(events) => {
                let mut map: BTreeMap<String, Agent> = BTreeMap::new();
                for ev in events {
                    match ev {
                        Ok(Event::Snapshot { agents }) => {
                            map = agents.into_iter().map(|a| (a.id.clone(), a)).collect();
                        }
                        Ok(Event::Agent { agent }) => {
                            map.insert(agent.id.clone(), *agent);
                        }
                        Ok(Event::Removed { id }) => {
                            map.remove(&id);
                        }
                        Err(_) => break,
                    }
                    let mut v: Vec<Agent> = map.values().cloned().collect();
                    v.sort_by_key(|a| a.created_at);
                    let j = ui::bar(&v);
                    if j != last {
                        emit(j.clone());
                        last = j;
                    }
                }
            }
            Err(_) => {
                let j = json!({"text": "", "tooltip": "drove daemon not running", "class": ["none"], "alt": "none"});
                if j != last {
                    emit(j.clone());
                    last = j;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

fn hooks(cmd: HooksCmd) -> Result<()> {
    let cfg = Config::load()?;
    let drove = cfg.drove_bin();
    match cmd {
        HooksCmd::Print { target } => {
            let v = match target {
                HookTarget::Claude => hi::claude_settings(&drove),
                HookTarget::Codex => hi::codex_hooks_file(&drove),
                HookTarget::Kiro => json!({"hooks": hi::kiro_hooks(&drove)}),
            };
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        HooksCmd::Install {
            target,
            agent,
            file,
        } => match target {
            HookTarget::Codex => {
                let p = file.unwrap_or_else(hi::codex_hooks_path);
                let changed = hi::install_codex(&drove, &p)?;
                report(&p, changed);
                eprintln!(
                    "Codex asks you to trust new hooks once: start codex and run /hooks.\n\
                     Agents spawned by drove also get `-c notify=…` so turn completion works regardless."
                );
            }
            HookTarget::Claude => {
                let p = file.unwrap_or_else(hi::claude_settings_path);
                let changed = hi::install_claude(&drove, &p)?;
                report(&p, changed);
                eprintln!(
                    "Only needed for Claude sessions started outside drove; `drove spawn` injects hooks via --settings."
                );
            }
            HookTarget::Kiro => {
                let results = match file {
                    Some(p) => {
                        let ours = hi::kiro_hooks(&drove);
                        let changed = hi::edit_json_file(&p, |r| hi::merge_kiro(r, &ours))?;
                        vec![(p, changed)]
                    }
                    None => hi::install_kiro(&drove, &hi::kiro_agents_dir(), agent.as_deref())?,
                };
                for (p, c) in results {
                    report(&p, c);
                }
            }
        },
    }
    Ok(())
}

fn report(p: &std::path::Path, changed: bool) {
    if changed {
        eprintln!(
            "updated {} (previous version saved as .drove-bak)",
            p.display()
        );
    } else {
        eprintln!("{} already up to date", p.display());
    }
}
