# drove-web

Local web dashboard for the drove daemon. One binary, one embedded HTML page
(vanilla JS/CSS, no build step, no CDN). Talks to the daemon only through the
socket protocol via the `drove-client` crate.

```sh
cd ui && cargo run -p drove-web -- [--listen 127.0.0.1:7878] [--socket PATH] [--unsafe-listen]
```

Open http://127.0.0.1:7878. Click a card to focus that agent's Hyprland window.
`Next` focuses whatever needs you most; `Preview` polls the screen text every 2s;
`Close` asks for confirmation; `Forget exited` clears dead agents. The header
and `document.title` (e.g. `(2!) drove`) show how many agents need you
(needs_input, or idle with attention). A red banner shows when the daemon is down.

Socket: `--socket`, else `$DROVE_SOCKET`, else the default from `drove-client`.

## Architecture

A single background thread holds ONE daemon subscription (own reconnect loop,
0.5s to 5s backoff; `drove_client::follow` reads `$DROVE_SOCKET` globally so it
is not used) and fans events out to all SSE clients via a broadcast channel.
It also keeps a copy of the current agent list so a new client immediately gets
`daemon` + `snapshot`. A client that lags gets a fresh `snapshot`. Actions open
a short-lived `drove_client::Client` connection per request.

## Security

- Binds loopback only; non-loopback `--listen` is refused unless `--unsafe-listen`.
  The API can type into your terminals and close windows, so treat exposure as
  remote code execution. There is no authentication.
- Every non-GET request needs header `X-Drove: 1` (else 403). Cross-origin pages
  cannot set it without a CORS preflight, which is never granted.
- The `Host` header must be localhost/127.x/::1 (DNS-rebinding guard; disabled by
  `--unsafe-listen`).
- Page is served with `X-Frame-Options: DENY`; all agent text is inserted with
  `textContent`.

## API

| route | body / query | response |
|---|---|---|
| `GET /` | | the page |
| `GET /events` | | SSE: `daemon` `{up}`, `snapshot` `{agents}`, `agent` `{agent}`, `removed` `{id}` |
| `GET /api/agents` | | `[Agent]`, sorted like `drove next` |
| `POST /api/agents/:id/focus` | | `{ok}` |
| `POST /api/next` | | `{agent}` (null if nothing needs you) |
| `POST /api/agents/:id/send` | `{text, enter}` | `{ok}` |
| `POST /api/agents/:id/close` | | `{ok}` |
| `POST /api/agents/:id/rename` | `{name}` | `{ok}` |
| `POST /api/agents/:id/forget` | | `{removed}` |
| `POST /api/forget-exited` | | `{removed}` |
| `GET /api/agents/:id/text?extent=screen` | `screen`, `all`, `last_cmd_output` | `{text}` |

Errors are `{error}`: 400 daemon rejected it, 403 CSRF/Host, 503 daemon unreachable.
`:id` may be an id, unique name or unique id prefix.

## Try it against the mock

```sh
python3 tools/mock-drove.py --socket /tmp/drove-mock.sock --log /tmp/req.log &
cd ui && cargo run -p drove-web -- --socket /tmp/drove-mock.sock
```

## Tests

```sh
cd ui
cargo test -p drove-web                       # spawns the mock daemon + server
cargo build -p drove-web
PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node web/tests/e2e.mjs   # needs `playwright` resolvable (global or npm i -D playwright-core is not required if global exists)
```

The e2e script loads the page, asserts cards render and that clicking one sends
`focus` to the mock, and writes `screenshots/{desktop-light,desktop-dark,mobile-390}.png`.
