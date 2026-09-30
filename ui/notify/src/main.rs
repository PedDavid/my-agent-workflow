use std::io::BufRead;
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{bail, Result};
use drove_client::Event;
use drove_notify::app::{App, Backend, Engine, Focuser, SocketFocuser, UiAction};
use drove_notify::backend::{DbusBackend, DryRun};
use drove_notify::config::{quiet_file_path, Config};

const USAGE: &str = "usage: drove-notify [--socket PATH] [--config PATH] [--dry-run]

  --socket PATH   drove daemon socket (default: $DROVE_SOCKET or the standard path)
  --config PATH   TOML config (default: ~/.config/drove/notify.toml, optional)
  --dry-run       print notifications as JSON lines instead of using D-Bus;
                  stdin accepts `focus ID` / `dismiss ID` to simulate actions
";

struct Args {
    socket: Option<PathBuf>,
    config: Option<PathBuf>,
    dry_run: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        socket: None,
        config: None,
        dry_run: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--socket" => {
                a.socket = Some(
                    it.next()
                        .ok_or_else(|| anyhow::anyhow!("--socket needs a value"))?
                        .into(),
                )
            }
            "--config" => {
                a.config = Some(
                    it.next()
                        .ok_or_else(|| anyhow::anyhow!("--config needs a value"))?
                        .into(),
                )
            }
            "--dry-run" => a.dry_run = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    Ok(a)
}

enum Msg {
    Event(Event),
    Ui(UiAction),
}

fn run_loop<B: Backend, F: Focuser>(mut app: App<B, F>, rx: mpsc::Receiver<Msg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Event(ev) => app.on_event(ev),
            Msg::Ui(a) => app.on_ui(a),
        }
    }
}

fn main() -> Result<()> {
    let args = parse_args()?;
    if let Some(s) = &args.socket {
        // Single-threaded here; `drove_client::follow` reads the env.
        std::env::set_var("DROVE_SOCKET", s);
    }
    let cfg = Config::load_or_default(args.config.as_deref())?;
    let engine = Engine::new(cfg.clone(), Some(quiet_file_path()));
    let focuser = SocketFocuser {
        socket: args.socket.clone(),
    };

    let (tx, rx) = mpsc::channel::<Msg>();
    let (ui_tx, ui_rx) = mpsc::channel::<UiAction>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for a in ui_rx {
                if tx.send(Msg::Ui(a)).is_err() {
                    break;
                }
            }
        });
    }
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let warned = std::cell::Cell::new(false);
            drove_client::follow(
                |ev| {
                    warned.set(false);
                    tx.send(Msg::Event(ev)).is_ok()
                },
                |e| {
                    if !warned.get() {
                        eprintln!("drove-notify: {e}; retrying");
                        warned.set(true);
                    }
                    true
                },
            );
        });
    }

    if args.dry_run {
        let ui_tx = ui_tx.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(|l| l.ok()) {
                let mut w = line.split_whitespace();
                let (Some(cmd), Some(id)) = (w.next(), w.next()) else {
                    continue;
                };
                let a = match cmd {
                    "focus" | "default" => UiAction::Focus(id.into()),
                    "dismiss" => UiAction::Dismiss(id.into()),
                    _ => continue,
                };
                if ui_tx.send(a).is_err() {
                    break;
                }
            }
        });
        run_loop(
            App::new(engine, DryRun::new(std::io::stdout(), &cfg), focuser),
            rx,
        );
    } else {
        run_loop(App::new(engine, DbusBackend::new(&cfg, ui_tx), focuser), rx);
    }
    Ok(())
}
