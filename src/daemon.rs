//! `drove daemon`: owns the agent store, listens for clients and Hyprland events.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, broadcast, mpsc};

use crate::adapters::{self, Source};
use crate::config::{Config, TerminalMode};
use crate::model::{Agent, Status, TermRef, new_id};
use crate::protocol::{Event, HookParams, Request, Response, SpawnParams};
use crate::state::{Change, Store};
use crate::term::{SpawnSpec, kitty};
use crate::wm::hyprland::{self, Hyprland};
use crate::wm::{WmEvent, id_from_class};
use crate::{hooks_install, now_ms, paths, shell, spawn};

/// Agents still `Starting` this long after spawn are not marked Exited by a reconcile.
const START_GRACE_MS: u64 = 60_000;

pub struct Daemon {
    cfg: Config,
    store: Mutex<Store>,
    tx: broadcast::Sender<Event>,
    hypr: Option<Hyprland>,
    socket2: Option<PathBuf>,
    socket: PathBuf,
    state_path: PathBuf,
    /// Ancestor PID → adopted agent id (hooks without `DROVE_AGENT_ID`).
    adopted_pids: Mutex<HashMap<i64, String>>,
    shutdown: Notify,
}

impl Daemon {
    pub fn new(cfg: Config) -> Arc<Self> {
        let socket2 = cfg.hyprland.socket2.clone().or_else(paths::hypr_socket2);
        let hypr = (cfg.hyprland.socket2.is_some() || paths::under_hyprland())
            .then(|| Hyprland::new(&cfg.hyprland));
        let (tx, _) = broadcast::channel(256);
        Arc::new(Daemon {
            store: Mutex::new(Store::new(cfg.kiro.stale_tool_secs * 1000)),
            tx,
            hypr,
            socket2,
            socket: paths::socket_path(),
            state_path: paths::state_file(),
            adopted_pids: Mutex::new(HashMap::new()),
            shutdown: Notify::new(),
            cfg,
        })
    }

    /// Broadcast changes and persist.
    fn commit(&self, store: &Store, changes: Vec<Change>) {
        if changes.is_empty() {
            return;
        }
        for c in changes {
            let ev = match c {
                Change::Updated(id) => match store.get(&id) {
                    Some(a) => Event::Agent {
                        agent: Box::new(a.clone()),
                    },
                    None => continue,
                },
                Change::Removed(id) => Event::Removed { id },
            };
            let _ = self.tx.send(ev);
        }
        if let Err(e) = store.save(&self.state_path) {
            tracing::warn!("saving state: {e:#}");
        }
    }

    fn with_store<T>(&self, f: impl FnOnce(&mut Store) -> (T, Vec<Change>)) -> T {
        let mut store = self.store.lock().unwrap();
        let (out, changes) = f(&mut store);
        self.commit(&store, changes);
        out
    }

    fn reconcile(&self) {
        let Some(h) = &self.hypr else { return };
        match h.clients() {
            Ok(clients) => {
                self.with_store(|s| ((), s.reconcile(&clients, now_ms(), START_GRACE_MS)))
            }
            Err(e) => tracing::warn!("reconcile: {e:#}"),
        }
    }

    fn hypr(&self) -> Result<&Hyprland> {
        self.hypr
            .as_ref()
            .ok_or_else(|| anyhow!("not running under Hyprland"))
    }

    fn agent(&self, key: &Value) -> Result<Agent> {
        let key = key.as_str().ok_or_else(|| anyhow!("missing agent id"))?;
        let store = self.store.lock().unwrap();
        let id = store.resolve(key)?;
        Ok(store.get(&id).unwrap().clone())
    }

    /// Handle one (non-streaming) request. Runs on a blocking thread.
    pub fn handle(&self, method: &str, p: &Value) -> Result<Value> {
        match method {
            "ping" => Ok(json!("pong")),
            "status" => Ok(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
                "socket": self.socket,
                "state": self.state_path,
                "hyprland": self.hypr.is_some(),
                "dispatch": self.hypr.as_ref().map(|h| h.mode().as_str()),
                "agents": self.store.lock().unwrap().agents.len(),
            })),
            "list" => Ok(serde_json::to_value(self.store.lock().unwrap().snapshot())?),
            "get" => Ok(serde_json::to_value(self.agent(&p["id"])?)?),
            "spawn" => {
                let params: SpawnParams = serde_json::from_value(p.clone())?;
                Ok(serde_json::to_value(self.spawn(params)?)?)
            }
            "focus" => {
                let a = self.agent(&p["id"])?;
                self.focus(&a)?;
                Ok(serde_json::to_value(a)?)
            }
            "next" => {
                let id = self.store.lock().unwrap().next();
                match id {
                    Some(id) => {
                        let a = self.store.lock().unwrap().get(&id).cloned().unwrap();
                        self.focus(&a)?;
                        Ok(serde_json::to_value(a)?)
                    }
                    None => Ok(Value::Null),
                }
            }
            "close" => {
                let a = self.agent(&p["id"])?;
                let w = a
                    .window
                    .as_ref()
                    .ok_or_else(|| anyhow!("{} has no window", a.name))?;
                self.hypr()?.close(&w.address)?;
                Ok(Value::Null)
            }
            "send_text" => {
                let a = self.agent(&p["id"])?;
                let term = a
                    .term
                    .as_ref()
                    .ok_or_else(|| anyhow!("{} has no terminal handle", a.name))?;
                let mut text = p["text"].as_str().unwrap_or("").to_string();
                if p["enter"].as_bool().unwrap_or(false) {
                    text.push('\r');
                }
                kitty::send_text(&self.cfg.terminal, term, &text)?;
                Ok(Value::Null)
            }
            "get_text" => {
                let a = self.agent(&p["id"])?;
                let term = a
                    .term
                    .as_ref()
                    .ok_or_else(|| anyhow!("{} has no terminal handle", a.name))?;
                let extent = p["extent"].as_str().unwrap_or("screen");
                Ok(json!(kitty::get_text(&self.cfg.terminal, term, extent)?))
            }
            "rename" => {
                let a = self.agent(&p["id"])?;
                let name = p["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("missing name"))?
                    .to_string();
                self.with_store(|s| {
                    let ag = s.agents.get_mut(&a.id).unwrap();
                    ag.name = name;
                    ag.updated_at = now_ms();
                    ((), vec![Change::Updated(a.id.clone())])
                });
                Ok(Value::Null)
            }
            "forget" => {
                let removed = if p["exited"].as_bool().unwrap_or(false) {
                    self.with_store(|s| {
                        let ch = s.forget_exited();
                        (ch.len(), ch)
                    })
                } else {
                    let a = self.agent(&p["id"])?;
                    self.with_store(|s| {
                        let ch = s.forget(&a.id);
                        (ch.len(), ch)
                    })
                };
                Ok(json!(removed))
            }
            "hook" => {
                let params: HookParams = serde_json::from_value(p.clone())?;
                self.hook(params)?;
                Ok(Value::Null)
            }
            _ => bail!("unknown method {method:?}"),
        }
    }

    fn focus(&self, a: &Agent) -> Result<()> {
        let w = a
            .window
            .as_ref()
            .ok_or_else(|| anyhow!("{} has no window", a.name))?;
        self.hypr()?.focus(&w.address)?;
        let addr = w.address.clone();
        self.with_store(|s| {
            s.focused = Some(addr);
            let ag = s.agents.get_mut(&a.id).unwrap();
            if ag.attention {
                ag.attention = false;
                ag.updated_at = now_ms();
                ((), vec![Change::Updated(a.id.clone())])
            } else {
                ((), vec![])
            }
        });
        Ok(())
    }

    fn hook(&self, p: HookParams) -> Result<()> {
        let source = Source::parse(&p.source).ok_or_else(|| anyhow!("bad source"))?;
        let hook = adapters::normalize(source, &p.payload);
        let now = now_ms();
        let id = match &p.agent_id {
            Some(id) => {
                let mut store = self.store.lock().unwrap();
                if !store.agents.contains_key(id) {
                    // Known id but lost state (e.g. state.json deleted): recreate.
                    let cwd = hook.cwd.clone().unwrap_or_default();
                    let base = spawn::default_name(source.kind().as_str(), &cwd);
                    let mut a =
                        Agent::new(id, &store.unique_name(&base), source.kind(), "", &cwd, now);
                    a.adopted = true;
                    let ch = store.insert(a);
                    self.commit(&store, vec![ch]);
                }
                id.clone()
            }
            None => match self.adopt(source, &hook, &p.pids)? {
                Some(id) => id,
                None => return Ok(()),
            },
        };
        self.with_store(|s| ((), s.apply_hook(&id, source, &hook, now)));
        Ok(())
    }

    /// Map a hook without `DROVE_AGENT_ID` to an agent by its ancestor PIDs.
    fn adopt(&self, source: Source, hook: &adapters::Hook, pids: &[i64]) -> Result<Option<String>> {
        if !self.cfg.hooks.adopt || pids.is_empty() {
            return Ok(None);
        }
        {
            let cache = self.adopted_pids.lock().unwrap();
            let store = self.store.lock().unwrap();
            for pid in pids {
                if let Some(id) = cache.get(pid)
                    && store.get(id).is_some_and(|a| a.status != Status::Exited)
                {
                    return Ok(Some(id.clone()));
                }
            }
        }
        let Some(h) = &self.hypr else { return Ok(None) };
        // Hook-less events (e.g. Ignore) are not worth a hyprctl call.
        if hook.signal == adapters::Signal::Ignore {
            return Ok(None);
        }
        let clients = h.clients()?;
        for pid in pids {
            let matching: Vec<_> = clients.iter().filter(|c| c.pid == *pid).collect();
            if matching.len() != 1 {
                // No window, or a shared terminal process with several windows: ambiguous.
                continue;
            }
            let c = matching[0];
            let now = now_ms();
            let id = self.with_store(|s| {
                if let Some(id) = id_from_class(&c.class).filter(|id| s.agents.contains_key(*id)) {
                    return (id.to_string(), vec![]);
                }
                if let Some(id) = s.by_address(&c.address) {
                    return (id, vec![]);
                }
                let mut id = new_id();
                while s.agents.contains_key(&id) {
                    id = new_id();
                }
                let cwd = hook.cwd.clone().unwrap_or_default();
                let base = spawn::default_name(source.kind().as_str(), &cwd);
                let mut a = Agent::new(&id, &s.unique_name(&base), source.kind(), "", &cwd, now);
                a.adopted = true;
                a.status = Status::Idle;
                a.window = Some(crate::model::WindowRef {
                    address: c.address.clone(),
                    workspace: c.workspace.clone(),
                    title: c.title.clone(),
                    class: c.class.clone(),
                });
                let ch = s.insert(a);
                (id, vec![ch])
            });
            self.adopted_pids.lock().unwrap().insert(*pid, id.clone());
            return Ok(Some(id));
        }
        Ok(None)
    }

    pub fn spawn(&self, p: SpawnParams) -> Result<Agent> {
        let profile = self.cfg.agents.get(&p.profile).cloned().ok_or_else(|| {
            let names: Vec<&str> = self.cfg.agents.keys().map(String::as_str).collect();
            anyhow!("no profile {:?} (have: {})", p.profile, names.join(", "))
        })?;
        let mut cwd = PathBuf::from(
            p.cwd
                .clone()
                .unwrap_or_else(|| paths::home().to_string_lossy().into_owned()),
        );
        if !cwd.is_dir() {
            bail!("{} is not a directory", cwd.display());
        }
        let mut worktree = None;
        if let Some(branch) = &p.worktree {
            cwd = spawn::ensure_worktree(&cwd, branch, &self.cfg.spawn.worktree_root)?;
            worktree = Some(cwd.to_string_lossy().into_owned());
        }
        let cwd_s = cwd.to_string_lossy().into_owned();
        let drove = self.cfg.drove_bin();
        let settings = if profile.kind == crate::model::AgentKind::Claude {
            Some(write_claude_settings(&drove)?)
        } else {
            None
        };
        let argv = spawn::agent_argv(&profile, &p.args, &drove, settings.as_deref())?;
        let workspace = p
            .workspace
            .clone()
            .or(profile.workspace.clone())
            .unwrap_or_else(|| self.cfg.spawn.workspace.clone());

        let now = now_ms();
        let agent = {
            let store = self.store.lock().unwrap();
            let mut id = new_id();
            while store.agents.contains_key(&id) {
                id = new_id();
            }
            let name = match &p.name {
                Some(n) => n.clone(),
                None => store.unique_name(&spawn::default_name(&p.profile, &cwd_s)),
            };
            let mut a = Agent::new(&id, &name, profile.kind, &p.profile, &cwd_s, now);
            a.worktree = worktree;
            a
        };
        let mut env = vec![
            ("DROVE_AGENT_ID".to_string(), agent.id.clone()),
            (
                "DROVE_SOCKET".to_string(),
                self.socket.to_string_lossy().into_owned(),
            ),
        ];
        env.extend(profile.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        let spec = SpawnSpec {
            id: agent.id.clone(),
            name: agent.name.clone(),
            cwd: cwd_s.clone(),
            env,
            argv,
        };

        let mut agent = agent;
        match self.cfg.terminal.mode {
            TerminalMode::Standalone => {
                let listen = format!("unix:{}", paths::kitty_socket_path(&agent.id).display());
                let argv = kitty::standalone_argv(&self.cfg.terminal, &spec, &listen);
                agent.term = Some(TermRef::Kitty {
                    socket: listen,
                    window_id: None,
                });
                self.with_store(|s| ((), vec![s.insert(agent.clone())]));
                let launched = match &self.hypr {
                    Some(h) => h.exec(&shell::join(&argv), &workspace),
                    None => spawn_detached(&argv, &cwd),
                };
                if let Err(e) = launched {
                    self.with_store(|s| ((), s.forget(&agent.id)));
                    return Err(e.context("launching terminal"));
                }
            }
            TerminalMode::Instance => {
                let to = self
                    .cfg
                    .terminal
                    .socket
                    .clone()
                    .context("terminal.mode = \"instance\" needs terminal.socket")?;
                self.with_store(|s| ((), vec![s.insert(agent.clone())]));
                match kitty::launch_instance(&self.cfg.terminal, &spec, &to) {
                    Ok(wid) => {
                        agent.term = Some(TermRef::Kitty {
                            socket: to,
                            window_id: wid,
                        });
                        let a = agent.clone();
                        self.with_store(|s| {
                            let term = a.term.clone();
                            let id = a.id.clone();
                            if let Some(x) = s.agents.get_mut(&id) {
                                x.term = term;
                            }
                            ((), vec![Change::Updated(id)])
                        });
                    }
                    Err(e) => {
                        self.with_store(|s| ((), s.forget(&agent.id)));
                        return Err(e.context("kitten @ launch"));
                    }
                }
            }
        }
        tracing::info!("spawned {} ({}) in {}", agent.name, agent.id, cwd_s);
        let store = self.store.lock().unwrap();
        Ok(store.get(&agent.id).cloned().unwrap_or(agent))
    }

    fn on_wm(&self, ev: Option<WmEvent>) {
        match ev {
            None => self.reconcile(),
            Some(ev) => self.with_store(|s| ((), s.apply_wm(&ev, now_ms()))),
        }
    }

    fn tick(&self) {
        self.with_store(|s| ((), s.tick(now_ms())));
    }
}

fn write_claude_settings(drove: &str) -> Result<PathBuf> {
    let dir = paths::data_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("claude-settings.json");
    let content = serde_json::to_string_pretty(&hooks_install::claude_settings(drove))?;
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
        let tmp = dir.join("claude-settings.json.tmp");
        std::fs::write(&tmp, &content)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

/// Not under Hyprland: start the terminal in its own session, reaped by a thread.
fn spawn_detached(argv: &[String], cwd: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", argv[0]))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

async fn write_line<T: serde::Serialize>(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    v: &T,
) -> Result<()> {
    let mut s = serde_json::to_string(v)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    Ok(())
}

async fn serve_conn(d: Arc<Daemon>, stream: UnixStream) -> Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_line(
                    &mut w,
                    &Response::err(Value::Null, format!("bad request: {e}")),
                )
                .await?;
                continue;
            }
        };
        match req.method.as_str() {
            "subscribe" => {
                let mut rx = d.tx.subscribe();
                write_line(&mut w, &Response::ok(req.id, Value::Null)).await?;
                let snap = Event::Snapshot {
                    agents: d.store.lock().unwrap().snapshot(),
                };
                write_line(&mut w, &snap).await?;
                loop {
                    tokio::select! {
                        ev = rx.recv() => match ev {
                            Ok(ev) => write_line(&mut w, &ev).await?,
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                let snap = Event::Snapshot { agents: d.store.lock().unwrap().snapshot() };
                                write_line(&mut w, &snap).await?;
                            }
                            Err(broadcast::error::RecvError::Closed) => return Ok(()),
                        },
                        l = lines.next_line() => match l {
                            Ok(Some(_)) => {}
                            _ => return Ok(()),
                        },
                    }
                }
            }
            "shutdown" => {
                write_line(&mut w, &Response::ok(req.id, Value::Null)).await?;
                d.shutdown.notify_one();
                return Ok(());
            }
            method => {
                let d2 = d.clone();
                let method = method.to_string();
                let params = req.params.clone();
                let res = tokio::task::spawn_blocking(move || d2.handle(&method, &params)).await?;
                let resp = match res {
                    Ok(v) => Response::ok(req.id, v),
                    Err(e) => Response::err(req.id, format!("{e:#}")),
                };
                write_line(&mut w, &resp).await?;
            }
        }
    }
    Ok(())
}

fn prepare_socket(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        if dir == paths::runtime_dir() {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            bail!("a drove daemon is already listening on {}", path.display());
        }
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn run(cfg: Config) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(run_async(cfg))
}

async fn run_async(cfg: Config) -> Result<()> {
    let d = Daemon::new(cfg);
    prepare_socket(&d.socket)?;
    let _ = std::fs::create_dir_all(paths::runtime_dir());
    {
        let mut s = d.store.lock().unwrap();
        if let Err(e) = s.load(&d.state_path) {
            tracing::warn!("ignoring unreadable {}: {e:#}", d.state_path.display());
        }
    }
    let listener =
        UnixListener::bind(&d.socket).with_context(|| format!("binding {}", d.socket.display()))?;
    tracing::info!("listening on {}", d.socket.display());

    {
        let d = d.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(h) = &d.hypr {
                tracing::info!("hyprland dispatch mode: {}", h.mode().as_str());
            }
            d.reconcile();
        })
        .await?;
    }

    if let Some(path) = d.socket2.clone() {
        let (etx, mut erx) = mpsc::channel(256);
        tokio::spawn(hyprland::event_stream(path, etx));
        let d = d.clone();
        tokio::spawn(async move {
            while let Some(ev) = erx.recv().await {
                let d = d.clone();
                let _ = tokio::task::spawn_blocking(move || d.on_wm(ev)).await;
            }
        });
    } else {
        tracing::warn!("no Hyprland instance found; window tracking disabled");
    }

    {
        let d = d.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(1));
            loop {
                iv.tick().await;
                d.tick();
            }
        });
    }

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            conn = listener.accept() => {
                let (stream, _) = conn?;
                let d = d.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_conn(d, stream).await {
                        tracing::debug!("connection: {e:#}");
                    }
                });
            }
            _ = d.shutdown.notified() => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        }
    }
    tracing::info!("shutting down");
    let _ = std::fs::remove_file(&d.socket);
    let store = d.store.lock().unwrap();
    let _ = store.save(&d.state_path);
    Ok(())
}
