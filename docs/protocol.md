# drove daemon protocol (for UI authors)

Everything a bar, dashboard, TUI, web page or notifier needs. Source of truth:
`src/protocol.rs`, `src/model.rs`, `src/daemon.rs`.

## Transport

- Unix stream socket: `$DROVE_SOCKET`, else `$XDG_RUNTIME_DIR/drove/drove.sock`
  (fallback `/tmp/drove-$UID/drove.sock` when `XDG_RUNTIME_DIR` is unset).
- Newline-delimited JSON (one object per line, UTF-8).
- One request → one response per line, in order. Open a **separate connection**
  for `subscribe`, because that connection becomes an event stream.

```jsonc
// request
{"id": 1, "method": "focus", "params": {"id": "ab12cd"}}
// response
{"id": 1, "ok": true, "result": { /* … */ }}
{"id": 1, "ok": false, "error": "no agent matching \"zz\""}
```

`id` is echoed back verbatim (any JSON value). `params` may be omitted when empty.

## Agent object

```jsonc
{
  "id": "ab12cd",                 // stable, 6 lowercase base32 chars; DROVE_AGENT_ID
  "name": "api-refactor",
  "kind": "claude",               // claude | codex | kiro | generic
  "profile": "claude",            // config profile name used to spawn
  "cwd": "/home/me/src/api",
  "status": "working",            // starting | idle | working | needs_input | exited
  "attention": true,              // finished or blocked while you weren't looking; cleared when its window is focused
  "detail": "Bash: cargo test",   // human-readable: current tool, prompt snippet, notification text…
  "maybe": true,                  // OPTIONAL, only present when true: status is a heuristic guess
  "session_id": "8f1c…",          // agent's own session id, if known
  "window": {                     // null when no Hyprland window is bound (yet / anymore)
    "address": "0x5f20cb695640",
    "workspace": "3",
    "title": "✳ api-refactor",
    "class": "drove-ab12cd"
  },
  "term": {"type": "kitty", "socket": "unix:/run/user/1000/drove/kitty-ab12cd.sock"}, // or null
  "worktree": "/home/me/src/api.worktrees/feat-x",   // or null
  "adopted": false,               // true = started by hand, discovered by drove
  "created_at": 1790729081000,    // unix ms
  "updated_at": 1790729999000,    // unix ms
  "last_hook_at": 1790729999000   // unix ms or null
}
```

Unknown fields may appear later; ignore them. Suggested visual mapping:

| status | meaning | suggested glyph |
|---|---|---|
| `starting` | spawned, no signal yet | `○` |
| `idle` | waiting for your prompt | `●` (accent it when `attention`) |
| `working` | agent is busy | spinner / `◐` |
| `needs_input` | blocked on you (permission, question) | `!` / red |
| `exited` | process/window gone; kept until `forget` | dim `×` |

Sort suggestion (what `drove next` uses): `needs_input` first, then `attention`
idle, then working, idle, starting, exited; ties by `updated_at` desc.

## Methods

| method | params | result |
|---|---|---|
| `ping` | – | `"pong"` |
| `status` | – | `{version, pid, socket, state, hyprland: bool, dispatch: "lua"\|"legacy"\|null, agents: n}` |
| `list` | – | `[Agent]` |
| `get` | `{id}` | `Agent` |
| `spawn` | `{profile, name?, cwd?, worktree?, workspace?, args?: [str]}` | `Agent` |
| `focus` | `{id}` | `Agent` (focuses its Hyprland window) |
| `next` | – | `Agent` focused, or `null` if nothing needs you |
| `close` | `{id}` | `null` (closes the window; agent becomes `exited`) |
| `send_text` | `{id, text, enter?: bool}` | `null` (types into the agent's terminal) |
| `get_text` | `{id, extent?: "screen"\|"all"\|"last_cmd_output"}` | `string` |
| `rename` | `{id, name}` | `null` |
| `forget` | `{id}` or `{exited: true}` | number removed |
| `subscribe` | – | see below |

Anywhere `{id}` is accepted, a unique **name** or unique id prefix works too.

## Subscribe

```
→ {"id":1,"method":"subscribe"}
← {"id":1,"ok":true,"result":null}
← {"event":"snapshot","agents":[Agent…]}        // always first
← {"event":"agent","agent":Agent}               // added or changed (full object)
← {"event":"removed","id":"ab12cd"}             // forgotten
← {"event":"snapshot","agents":[…]}             // again if you fell behind: replace your state
```

Treat every `snapshot` as "replace all". Keep the connection open; reconnect
with backoff (e.g. 0.5s → 5s) when it drops, since the daemon may restart. Any line
you send on a subscribed connection is ignored; closing it ends the stream.

## Testing without Hyprland

`tools/mock-drove.py` speaks this protocol with fake agents that change state
every few seconds, and logs the requests it gets:

```sh
python3 tools/mock-drove.py --socket /tmp/drove-mock.sock &
DROVE_SOCKET=/tmp/drove-mock.sock your-ui
```
