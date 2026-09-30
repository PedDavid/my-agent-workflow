//! drove-web: a local web dashboard for the drove daemon.
//!
//! Design: a single background thread keeps ONE daemon subscription and fans
//! events out to all SSE clients through a broadcast channel (plus a shared
//! copy of the current state so new clients get a snapshot immediately).
//! Actions open a short-lived `drove_client::Client` connection per request
//! on a blocking thread.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as Sse, KeepAlive};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use drove_client::{apply, sort_agents, Agent, Client, Error, Event, Subscription};
use futures_util::stream::{self, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;

const INDEX: &str = include_str!("index.html");

#[derive(Clone, Debug)]
enum Msg {
    Daemon(bool),
    Event(Event),
}

#[derive(Default)]
struct Shared {
    up: bool,
    agents: Vec<Agent>,
}

#[derive(Clone)]
pub struct App {
    socket: Arc<PathBuf>,
    shared: Arc<Mutex<Shared>>,
    tx: broadcast::Sender<Msg>,
    check_host: bool,
}

impl App {
    /// Create the app and start the daemon-following thread.
    pub fn new(socket: PathBuf) -> App {
        let (tx, _) = broadcast::channel(256);
        let app = App {
            socket: Arc::new(socket),
            shared: Arc::new(Mutex::new(Shared::default())),
            tx,
            check_host: true,
        };
        let a = app.clone();
        std::thread::spawn(move || a.follow());
        app
    }

    /// Disable the Host-header allow-list (needed for `--unsafe-listen`).
    pub fn allow_any_host(mut self) -> App {
        self.check_host = false;
        self
    }

    /// Whether the daemon subscription is currently established.
    pub fn is_up(&self) -> bool {
        self.shared.lock().unwrap().up
    }

    fn set_up(&self, up: bool) {
        let mut s = self.shared.lock().unwrap();
        if s.up != up {
            s.up = up;
            let _ = self.tx.send(Msg::Daemon(up));
        }
    }

    /// Own reconnect loop (rather than `drove_client::follow`) because that
    /// one reads `$DROVE_SOCKET` globally, which breaks per-instance sockets.
    fn follow(&self) {
        let mut backoff = Duration::from_millis(500);
        loop {
            if let Ok(sub) = Subscription::open_at(&self.socket) {
                backoff = Duration::from_millis(500);
                self.set_up(true);
                for ev in sub {
                    let Ok(ev) = ev else { break };
                    let mut s = self.shared.lock().unwrap();
                    apply(&mut s.agents, ev.clone());
                    let _ = self.tx.send(Msg::Event(ev));
                }
            }
            self.set_up(false);
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    }

    async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Client) -> drove_client::Result<T> + Send + 'static,
    ) -> Result<T, ApiError> {
        let sock = self.socket.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = Client::connect_to(&sock)?;
            f(&mut c)
        })
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(ApiError::from)
    }
}

pub struct ApiError(StatusCode, String);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        match e {
            Error::Daemon(m) => ApiError(StatusCode::BAD_REQUEST, m),
            other => ApiError(StatusCode::SERVICE_UNAVAILABLE, other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn event_parts(msg: &Msg) -> (&'static str, Value) {
    match msg {
        Msg::Daemon(up) => ("daemon", json!({"up": up})),
        Msg::Event(ev) => {
            let name = match ev {
                Event::Snapshot { .. } => "snapshot",
                Event::Agent { .. } => "agent",
                Event::Removed { .. } => "removed",
            };
            (name, serde_json::to_value(ev).unwrap_or(Value::Null))
        }
    }
}

fn sse(msg: &Msg) -> Result<Sse, Infallible> {
    let (name, data) = event_parts(msg);
    Ok(Sse::default().event(name).data(data.to_string()))
}

async fn events(State(app): State<App>) -> impl IntoResponse {
    // Subscribe and copy state under the lock so no event is lost or doubled.
    let (rx, up, agents) = {
        let s = app.shared.lock().unwrap();
        (app.tx.subscribe(), s.up, s.agents.clone())
    };
    let mut initial = vec![sse(&Msg::Daemon(up))];
    if up {
        initial.push(sse(&Msg::Event(Event::Snapshot { agents })));
    }
    let shared = app.shared.clone();
    let live = BroadcastStream::new(rx).map(move |r| match r {
        Ok(m) => sse(&m),
        // Fell behind: replace the client's state wholesale.
        Err(BroadcastStreamRecvError::Lagged(_)) => {
            let agents = shared.lock().unwrap().agents.clone();
            sse(&Msg::Event(Event::Snapshot { agents }))
        }
    });
    let s: std::pin::Pin<Box<dyn Stream<Item = Result<Sse, Infallible>> + Send>> =
        Box::pin(stream::iter(initial).chain(live));
    (
        [(header::CACHE_CONTROL, "no-store")],
        axum::response::Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))),
    )
}

async fn index() -> impl IntoResponse {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Html(INDEX),
    )
}

async fn list(State(app): State<App>) -> ApiResult {
    let mut agents = app.call(|c| c.list()).await?;
    sort_agents(&mut agents);
    Ok(Json(serde_json::to_value(agents).unwrap()))
}

async fn focus(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.call(move |c| c.focus(&id)).await?;
    Ok(Json(json!({"ok": true})))
}

async fn next(State(app): State<App>) -> ApiResult {
    let a = app.call(|c| c.focus_next()).await?;
    Ok(Json(json!({"agent": a})))
}

#[derive(Deserialize)]
struct SendBody {
    text: String,
    #[serde(default)]
    enter: bool,
}

async fn send(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(b): Json<SendBody>,
) -> ApiResult {
    app.call(move |c| c.send_text(&id, &b.text, b.enter))
        .await?;
    Ok(Json(json!({"ok": true})))
}

async fn close(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.call(move |c| c.close(&id)).await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

async fn rename(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(b): Json<RenameBody>,
) -> ApiResult {
    app.call(move |c| c.rename(&id, &b.name)).await?;
    Ok(Json(json!({"ok": true})))
}

async fn forget(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let n = app.call(move |c| c.forget(&id)).await?;
    Ok(Json(json!({"removed": n})))
}

async fn forget_exited(State(app): State<App>) -> ApiResult {
    let n = app.call(|c| c.forget_exited()).await?;
    Ok(Json(json!({"removed": n})))
}

#[derive(Deserialize)]
struct TextQuery {
    extent: Option<String>,
}

async fn text(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<TextQuery>,
) -> ApiResult {
    let extent = q.extent.unwrap_or_else(|| "screen".into());
    if !matches!(extent.as_str(), "screen" | "all" | "last_cmd_output") {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!("bad extent {extent:?}"),
        ));
    }
    let t = app.call(move |c| c.get_text(&id, &extent)).await?;
    Ok(Json(json!({"text": t})))
}

/// DNS-rebinding guard: only answer to loopback host names.
fn host_ok(h: Option<&HeaderValue>) -> bool {
    let Some(h) = h.and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let host = match h.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => h.rsplit_once(':').map_or(h, |(a, _)| a),
    };
    host == "localhost"
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|i| i.is_loopback())
}

/// CSRF guard: any state-changing request must carry `X-Drove: 1`. Browsers
/// can't add custom headers cross-origin without a CORS preflight, which we
/// never grant.
async fn csrf(State(app): State<App>, req: Request, next: Next) -> Response {
    if app.check_host && !host_ok(req.headers().get(header::HOST)) {
        return ApiError(StatusCode::FORBIDDEN, "bad Host header".into()).into_response();
    }
    let safe = matches!(req.method().as_str(), "GET" | "HEAD" | "OPTIONS");
    if !safe && req.headers().get("x-drove") != Some(&HeaderValue::from_static("1")) {
        return ApiError(StatusCode::FORBIDDEN, "missing X-Drove: 1 header".into()).into_response();
    }
    next.run(req).await
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/events", get(events))
        .route("/api/agents", get(list))
        .route("/api/agents/{id}/focus", post(focus))
        .route("/api/agents/{id}/send", post(send))
        .route("/api/agents/{id}/close", post(close))
        .route("/api/agents/{id}/rename", post(rename))
        .route("/api/agents/{id}/forget", post(forget))
        .route("/api/agents/{id}/text", get(text))
        .route("/api/next", post(next))
        .route("/api/forget-exited", post(forget_exited))
        .layer(middleware::from_fn_with_state(app.clone(), csrf))
        .with_state(app)
}

/// Reject non-loopback listen addresses unless explicitly allowed.
pub fn check_listen(addr: &std::net::SocketAddr, allow_unsafe: bool) -> Result<(), String> {
    if addr.ip().is_loopback() || allow_unsafe {
        Ok(())
    } else {
        Err(format!(
            "refusing to listen on non-loopback address {addr} (the API can type into your terminals); pass --unsafe-listen to override"
        ))
    }
}
