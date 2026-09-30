//! Background threads: event follower, action executor, preview poller.
//! Everything talks to the daemon through `drove-client`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use drove_client::{Client, Error, Event, Subscription};

use crate::app::{Action, Msg};

/// Like `drove_client::follow` but for an explicit socket path, so it does not
/// depend on the process-global `DROVE_SOCKET` (needed for tests and for
/// running several followers). Also reports successful connections.
pub fn follow_at(
    path: &Path,
    mut on_connect: impl FnMut() -> bool,
    mut on_event: impl FnMut(Event) -> bool,
    mut on_disconnect: impl FnMut(&Error) -> bool,
) {
    let mut backoff = Duration::from_millis(500);
    loop {
        match Subscription::open_at(path) {
            Ok(sub) => {
                backoff = Duration::from_millis(500);
                if !on_connect() {
                    return;
                }
                for ev in sub {
                    match ev {
                        Ok(ev) => {
                            if !on_event(ev) {
                                return;
                            }
                        }
                        Err(e) => {
                            if !on_disconnect(&e) {
                                return;
                            }
                            break;
                        }
                    }
                }
                let eof = Error::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "daemon closed stream",
                ));
                if !on_disconnect(&eof) {
                    return;
                }
            }
            Err(e) => {
                if !on_disconnect(&e) {
                    return;
                }
            }
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

/// Run one action against the daemon; the string is a human-readable outcome.
pub fn run_action(client: &mut Client, action: &Action) -> Result<String, String> {
    exec(client, action).map_err(|e| e.to_string())
}

fn exec(client: &mut Client, action: &Action) -> Result<String, Error> {
    match action {
        Action::Focus(id) => client.focus(id).map(|_| String::new()),
        Action::FocusNext => client.focus_next().map(|a| match a {
            Some(a) => format!("focused {}", a.name),
            None => "nothing needs you".to_string(),
        }),
        Action::Send { id, text } => client.send_text(id, text, true).map(|_| "sent".to_string()),
        Action::Rename { id, name } => client
            .rename(id, name)
            .map(|_| format!("renamed to {name}")),
        Action::Close(id) => client.close(id).map(|_| "closed".to_string()),
        Action::ForgetExited => client
            .forget_exited()
            .map(|n| format!("forgot {n} exited agent(s)")),
        Action::Quit => Ok(String::new()),
    }
}

/// Start the follower thread: pushes events / connection state into `tx`.
pub fn spawn_follower(path: PathBuf, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let (t1, t2, t3) = (tx.clone(), tx.clone(), tx);
        follow_at(
            &path,
            move || t1.send(Msg::Connected).is_ok(),
            move |ev| t2.send(Msg::Event(ev)).is_ok(),
            move |e| t3.send(Msg::Disconnected(e.to_string())).is_ok(),
        );
    });
}

/// Start the executor thread: runs actions off the UI thread, reports results.
pub fn spawn_executor(path: PathBuf, rx: Receiver<Action>, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let mut client: Option<Client> = None;
        for action in rx {
            if action == Action::Quit {
                break;
            }
            let mut result = Err(String::new());
            // One retry with a fresh connection if the cached one went stale.
            for _ in 0..2 {
                if client.is_none() {
                    match Client::connect_to(&path) {
                        Ok(c) => client = Some(c),
                        Err(e) => {
                            result = Err(e.to_string());
                            break;
                        }
                    }
                }
                match exec(client.as_mut().unwrap(), &action) {
                    Err(Error::Io(e)) => {
                        client = None;
                        result = Err(Error::Io(e).to_string());
                    }
                    other => {
                        result = other.map_err(|e| e.to_string());
                        break;
                    }
                }
            }
            if tx.send(Msg::ActionDone(result)).is_err() {
                break;
            }
        }
    });
}

/// Shared "which agent is selected" cell read by the preview poller.
pub type Selection = Arc<Mutex<Option<String>>>;

/// Poll `get_text(id, "screen")` for the selected agent: immediately when the
/// selection changes, then every `interval`.
pub fn spawn_preview(path: PathBuf, sel: Selection, interval: Duration, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let mut client: Option<Client> = None;
        let mut last: Option<(String, Instant)> = None;
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let Some(id) = sel.lock().ok().and_then(|g| g.clone()) else {
                last = None;
                continue;
            };
            let due = match &last {
                Some((lid, at)) => lid != &id || at.elapsed() >= interval,
                None => true,
            };
            if !due {
                continue;
            }
            last = Some((id.clone(), Instant::now()));
            if client.is_none() {
                client = Client::connect_to(&path).ok();
            }
            let msg = match client.as_mut().map(|c| c.get_text(&id, "screen")) {
                Some(Ok(text)) => Msg::Preview { id, text },
                Some(Err(e)) => {
                    if matches!(e, Error::Io(_)) {
                        client = None;
                    }
                    Msg::PreviewError {
                        id,
                        error: e.to_string(),
                    }
                }
                None => Msg::PreviewError {
                    id,
                    error: "daemon unreachable".into(),
                },
            };
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
}
