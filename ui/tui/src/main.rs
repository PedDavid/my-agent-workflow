use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event as TermEvent, KeyEventKind};
use drove_tui::app::{Action, App, Msg};
use drove_tui::{ui, worker};

const USAGE: &str = "usage: drove-tui [--socket PATH]\n\nTerminal dashboard for the drove daemon. Press ? inside for keys.";

fn parse_args() -> Result<Option<PathBuf>, String> {
    let mut socket = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" | "-s" => {
                socket = Some(PathBuf::from(args.next().ok_or("--socket needs a path")?))
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    Ok(socket)
}

fn main() {
    let socket = match parse_args() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if let Some(s) = &socket {
        // Single-threaded here: no other thread has started yet.
        std::env::set_var("DROVE_SOCKET", s);
    }
    let path = socket.unwrap_or_else(drove_client::socket_path);

    let mut terminal = ratatui::init();
    let res = run(&mut terminal, path);
    ratatui::restore();
    if let Err(e) = res {
        eprintln!("drove-tui: {e}");
        std::process::exit(1);
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, path: PathBuf) -> std::io::Result<()> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let (atx, arx) = mpsc::channel::<Action>();
    let sel: worker::Selection = Arc::new(Mutex::new(None));
    worker::spawn_follower(path.clone(), tx.clone());
    worker::spawn_executor(path.clone(), arx, tx.clone());
    worker::spawn_preview(path, sel.clone(), Duration::from_secs(2), tx);

    let mut app = App::new();
    while !app.should_quit {
        while let Ok(m) = rx.try_recv() {
            app.handle_msg(m);
        }
        if let Ok(mut g) = sel.lock() {
            *g = app.selected_id.clone();
        }
        terminal.draw(|f| ui::draw(f, &app))?;
        if event::poll(Duration::from_millis(100))? {
            if let TermEvent::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Release {
                    for a in app.handle_key(k) {
                        let _ = atx.send(a);
                    }
                }
            }
        } else {
            app.handle_msg(Msg::Tick);
        }
    }
    Ok(())
}
