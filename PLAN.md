# drove — agent herding built into the window manager

Goal: something like [herdr](https://github.com/ogulcancelik/herdr), but with no
multiplexer. Every agent gets its own **Hyprland window**, in the kitty
philosophy: the WM does the layout and the terminal is just a terminal. A small daemon
tracks agent state from agent hooks and exposes it for dashboards, pickers and
bars.

Non-goals for v1: session persistence (zmx), ghostty backend, Quickshell UI. The
design must leave room for all three (see "Seams").

## Decisions (already made — don't re-litigate)

- Language: **Rust**, single binary `drove` with subcommands. The daemon is `drove daemon`.
- v1 agents: **Claude Code, Codex, kiro-cli** (+ `generic` = any command, no hooks).
- v1 dashboard: **picker** (`drove pick` via fuzzel/wofi/rofi dmenu mode) + `drove ls`.
  A Waybar-compatible `drove bar` JSON stream is cheap and also feeds a future Quickshell widget.
- Terminal: **kitty** first. Ghostty later behind the same interface.
- WM: **Hyprland ≥ 0.55 with Lua config** (user runs Lua config).

## Verified facts about the external tools (from their source, Sept 2026)

### Hyprland (0.56, Lua config)
- Sockets: `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock` (requests)
  and `.socket2.sock` (events, lines of `EVENT>>DATA`).
- Under Lua config, `hyprctl dispatch X` is evaluated as `return hl.dispatch(X)` — **X must be
  a Lua expression**; the old `dispatch exec [rules] cmd` syntax fails. Examples:
  - `hyprctl dispatch 'hl.dsp.focus({ window = "address:0x55d1c0ffee" })'`
  - `hyprctl dispatch 'hl.dsp.exec_cmd("kitty --class drove-ab12cd", { workspace = "3 silent" })'`
    (rule table keys: `workspace`, `monitor`, `float`, `move`, `size`, `pin`, …)
  - `hyprctl dispatch 'hl.dsp.window.close({ window = "address:0x…" })'` (close takes an optional `{window=…}`; verify, else focus+close)
  - `hyprctl dispatch 'hl.dsp.no_op()'` — succeeds only under a Lua config (detection probe).
- Window selectors (strings): `address:0x…`, `class:<regex>`, `title:<regex>`, `pid:<n>`, `tag:<regex>`.
- `hyprctl -j clients` → array with `address` ("0x…"), `class`, `initialClass`, `title`,
  `pid`, `workspace {id,name}`, `tags`, `focusHistoryID`, …
- `hyprctl eval '<lua>'` exists under Lua config.
- socket2 events we care about (address in events is hex **without** `0x`):
  - `openwindow>>ADDR,WORKSPACENAME,CLASS,TITLE`
  - `closewindow>>ADDR`
  - `activewindowv2>>ADDR` (empty data = no focus)
  - `windowtitlev2>>ADDR,TITLE`
  - `movewindowv2>>ADDR,WSID,WSNAME`
  - `urgent>>ADDR`
- Keep a config toggle `hyprland.dispatch = "auto" | "lua" | "legacy"`. `auto` probes with `hl.dsp.no_op()` once.
  `legacy` emits `dispatch focuswindow address:0x…` / `dispatch exec [workspace 3 silent] cmd`.

### kitty (0.49) remote control
- Standalone per-agent instance (**default mode**):
  `kitty --class drove-<id> --title <name> --directory <cwd> --listen-on unix:<runtime>/drove/kitty-<id>.sock -o allow_remote_control=socket-only env DROVE_AGENT_ID=<id> DROVE_SOCKET=<sock> <agent argv…>`
  → Hyprland sees app id / class `drove-<id>`. Control it with `kitten @ --to unix:<that sock> …` (single window, no `--match` needed).
- Shared-instance mode (optional config): `kitten @ --to <user sock> launch --type=os-window --os-window-class drove-<id> --os-window-title <name> --cwd <cwd> --var drove_id=<id> --env DROVE_AGENT_ID=<id> -- <argv>` prints the new kitty window id; later calls use `--match id:<n>`.
- `kitten @ ls` JSON: os windows → tabs → windows with `id, pid, cwd, cmdline, title, env, user_vars, foreground_processes, at_prompt, needs_attention, has_activity_since_last_focus, is_focused …`.
- `kitten @ send-text [--match …] -- text`, `kitten @ get-text [--match …] --extent screen|all|last_cmd_output`.

### Claude Code hooks
- Inject per spawn, **do not touch user settings**: `claude --settings <path-to-generated-json> [user args]`.
  Generated file: `{"hooks": {"<Event>": [{"matcher": "", "hooks": [{"type":"command","command":"<abs drove path> hook claude"}]}]}}`
  for events `SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, PermissionRequest, Notification, Stop, SubagentStop, SessionEnd`.
- Stdin JSON has `hook_event_name, session_id, transcript_path, cwd`, plus `prompt` (UserPromptSubmit),
  `tool_name/tool_input` (tool events), `message`/`notification_type` (Notification: e.g. `permission_prompt`, `idle_prompt`).
- **stdout of SessionStart/UserPromptSubmit is injected into the model context → `drove hook` must print nothing and exit 0, always, fast.**

### Codex hooks
- Codex now has Claude-compatible hooks: events `SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest,
  PostToolUse, PreCompact, PostCompact, SubagentStart, SubagentStop, Stop, SessionEnd`; stdin JSON with `hook_event_name,
  session_id, cwd, turn_id, tool_name, tool_input, prompt, last_assistant_message …` → **same normalizer as Claude**.
- User hooks live in `~/.codex/hooks.json` (same shape as Claude's `hooks` object) and must be **trusted once** in Codex (`/hooks`). So:
  - `drove hooks install codex` merges our entries into `~/.codex/hooks.json` (idempotent, backup first) and tells the user to trust them.
  - Always-works fallback injected at spawn: `codex -c 'notify=["<abs drove>","hook","codex-notify"]'`. Codex appends a JSON arg:
    `{"type":"agent-turn-complete","thread-id":…,"turn-id":…,"cwd":…,"input-messages":[…],"last-assistant-message":…}` → Idle.

### kiro-cli hooks
- Hooks live in the agent config JSON (`~/.kiro/agents/<name>.json`, key `hooks`), shape:
  `{"hooks": {"agentSpawn":[{"command":"…"}], "userPromptSubmit":[…], "preToolUse":[{"matcher":"*","command":"…"}], "postToolUse":[…], "stop":[…]}}`.
- Stdin: `{"hook_event_name":"agentSpawn|userPromptSubmit|preToolUse|postToolUse|stop","cwd":…, "prompt"?, "tool_name"?, "tool_input"?, "tool_response"?}`. No session id, no permission hook.
- stdout of agentSpawn/userPromptSubmit is added to context → print nothing.
- `drove hooks install kiro [--agent <name>]` merges hooks into the named agent file (default: all files in `~/.kiro/agents/`, or print the snippet with `drove hooks print kiro`).
- Needs-input for kiro isn't observable via hooks. v1: `preToolUse` without `postToolUse` for > N s (config, default 4s) → status `needs_input` (flag `maybe: true` in detail). Optional later: screen probe via `kitten @ get-text`.

## Architecture

```
agent hooks ──► drove hook <src> ──(unix socket, fire-and-forget)──► drove daemon ◄── hyprland socket2 events
                                                                         │  ▲
                                             hyprctl / kitten @ ◄────────┘  │ NDJSON req/resp + subscribe
                                                                            │
                                   drove ls / pick / focus / next / spawn / bar / watch / (future: Quickshell)
```

### Crate layout (single crate, lib + bin)
- `config.rs` — `~/.config/drove/config.toml` (all optional; sane defaults). Agent profiles:
  ```toml
  [terminal]            # kind = "kitty"; mode = "standalone" | "instance"; kitty = "kitty"; kitten = "kitten"; socket = "unix:@mykitty" (instance mode)
  [hyprland]            # hyprctl = "hyprctl"; dispatch = "auto"; socket2 = (override path, used by tests)
  [picker]              # command = ["fuzzel", "--dmenu"]
  [spawn]               # workspace = "" (e.g. "special:agents" or "3 silent"); worktree_root = "{repo}/../{repo_name}.worktrees/{branch}"
  [agents.claude]       # kind = "claude"; command = ["claude"]
  [agents.codex]        # kind = "codex";  command = ["codex"]
  [agents.kiro]         # kind = "kiro";   command = ["kiro-cli", "chat"]
  ```
  Users can add `[agents.foo] kind = "claude", command = ["claude", "--model", "opus"]`.
- `paths.rs` — XDG: runtime `$XDG_RUNTIME_DIR/drove/` (daemon socket `drove.sock`, kitty sockets), state `$XDG_STATE_HOME/drove/state.json`,
  data `$XDG_DATA_HOME/drove/` (generated claude settings). Env overrides `DROVE_SOCKET`, `DROVE_CONFIG`, `DROVE_STATE_DIR` for tests.
- `model.rs` — `Agent { id, name, kind, profile, cwd, status, attention, detail, session_id, window: Option<WindowRef{address, workspace, title, class}>, term: Option<TermRef>, worktree, adopted, created_at, updated_at, last_hook_at }`,
  `Status = Starting | Idle | Working | NeedsInput | Exited`. Timestamps = unix seconds/millis (no chrono needed).
- `adapters.rs` — pure fns: `(source, payload JSON) -> Signal` where `Signal = SessionStarted{session_id} | PromptSubmitted{prompt} | ToolStart{name} | ToolEnd{name} | NeedsInput{msg} | TurnDone{last_message} | SessionEnded | Ignore`.
  Mapping: Claude/Codex by `hook_event_name`; Notification with `notification_type == "idle_prompt"` → TurnDone-ish idle, else NeedsInput; kiro camelCase names; codex-notify `agent-turn-complete` → TurnDone.
  **Heavily unit-tested with fixture JSON.**
- `state.rs` — `Store` applying Signals + WM events, producing change events. Rules:
  - SessionStarted → Idle; Prompt/Tool* → Working (detail = tool name / prompt snippet); NeedsInput → NeedsInput (attention = true);
    TurnDone → Idle, attention = true **unless its window is currently focused**; SessionEnded or window closed → Exited.
  - `activewindowv2` on an agent's window → attention = false.
  - Persist to state.json (atomic write via tmp+rename) on every change (debounce ok).
- `lua.rs` — `lua_str(s) -> String` producing a safe Lua string literal (escape `\\ " \n \r \0` and control chars, or long brackets with a level not present). Tested.
- `shell.rs` — POSIX shell quoting for building `exec_cmd` strings. Tested.
- `wm/hyprland.rs` — request via `hyprctl` binary (configurable path — this is how tests inject fakes); event stream by connecting to socket2 (tokio UnixStream, line reader, auto-reconnect with backoff). Parse events into `WmEvent`. Functions: `clients()`, `focus(addr)`, `close(addr)`, `exec(cmdline, rules)`, `probe_lua()`.
- `term/kitty.rs` — build spawn argv (standalone/instance), `send_text`, `get_text`, `ls`. All via configurable `kitty`/`kitten` binaries.
- `protocol.rs` — NDJSON. Request `{"id":1,"method":"list","params":{}}` → `{"id":1,"ok":true,"result":…}` / `{"id":1,"ok":false,"error":"…"}`.
  Methods: `ping, list, get, spawn, focus, next, close, send_text, get_text, rename, forget, hook, subscribe, shutdown`.
  `subscribe` → connection turns into a stream: first `{"event":"snapshot","agents":[…]}`, then `{"event":"agent","agent":{…}}` / `{"event":"removed","id":…}`.
  **This event stream is the public API for Quickshell/other UIs — document it in README.**
- `daemon.rs` — tokio; `Arc<Mutex<Store>>` + `broadcast` channel for events; one task per client connection; one task for socket2.
  On start: load state.json, reconcile with `hyprctl -j clients` (match class `drove-<id>`; agents without windows → Exited; unknown `drove-*` windows → re-adopt minimal agent).
  Spawn: resolve profile → id (6 lowercase base32 chars) → optional worktree → build agent argv with injections (claude `--settings`, codex `-c notify=…`) → build terminal argv → launch through Hyprland `exec_cmd` (so the WM owns the process and applies workspace rules; fallback: `setsid` spawn when not under Hyprland) → register agent `Starting` → `openwindow` with class `drove-<id>` binds the address.
  Kiro stale-tool timer: periodic tick (1s) checks Working agents of kind kiro.
  **Hook adoption** (nice-to-have, after core): hook without `DROVE_AGENT_ID` → client sends its ancestor PID chain (read `/proc/<pid>/stat`); daemon matches any pid against `hyprctl clients` pids → adopted agent keyed by (source, session_id or window). Enables tracking agents started by hand.
- `client.rs` — **blocking std** UnixStream client (no tokio) for CLI commands; `drove hook` must not start a tokio runtime, must use short timeouts (~200ms), never print to stdout, and always exit 0 (daemon down = silently drop).
- `cli.rs`/`main.rs` — clap:
  - `drove daemon`
  - `drove spawn <profile> [--name N] [--cwd DIR] [--worktree BRANCH] [--workspace WS] [-- extra agent args…]` (prints id)
  - `drove ls [--json]` — table: status icon, name, id, kind, status (+ attention marker), detail, workspace, cwd
  - `drove focus <id|name>`; `drove next` (priority: NeedsInput → attention+Idle → oldest attention); `drove close <id>`; `drove forget <id|--exited>`
  - `drove send <id> <text…>` (kitty send-text, add `\r` with `--enter`); `drove text <id>` (get-text)
  - `drove pick` — lines `"<icon> <name>  <status>  <detail>  [<id>]"`, pipe to picker cmd, parse `[id]` from selection, focus
  - `drove bar` — long-running Waybar custom module JSON lines: `{"text":"…","tooltip":"…","class":["needs-input"|"working"|"idle"|"none"]}`
  - `drove watch` — print raw event stream
  - `drove hook <claude|codex|kiro|codex-notify> [json-arg]`
  - `drove hooks print <codex|kiro|claude>` / `drove hooks install <codex|kiro>`
  - `drove status` (daemon ping + hyprland mode detected)
- `contrib/hyprland.lua` — snippet: autostart `drove daemon`, binds (SUPER+A pick, SUPER+N next, SUPER+SHIFT+A spawn claude in a new window), optional window rules for `drove-.*` (e.g. `hl.on` or rule API — check example/hyprland.lua conventions in Hyprland repo).
- `contrib/drove.service` (systemd user unit, `PartOf=graphical-session.target`) as alternative to autostart.
- `contrib/waybar.jsonc` snippet.

### Seams (keep, don't implement)
- Agent identity is `DROVE_AGENT_ID`, never the window. A window is a *view* (`window: Option<WindowRef>`), so a future session backend (zmx) can re-attach new windows to the same agent.
- `TermRef` is an enum (`Kitty{socket, window_id}`; later `Ghostty…`).
- The WM layer is a module with a narrow surface (clients/focus/close/exec/events) so niri/sway could be added later.

## Testing
1. Unit tests: adapters (fixture payloads for all 3 agents), state transitions, lua/shell quoting, socket2 line parsing, `hyprctl -j clients` parsing, kitty argv building, picker line parsing.
2. Integration test (`tests/e2e.rs`): start the daemon in-process or as a subprocess with env overrides pointing at
   - a fake `hyprctl` script (records argv to a file, answers `-j clients` from a JSON file, answers `dispatch hl.dsp.no_op()` with `ok`),
   - a fake socket2 (test binds the unix socket and writes `openwindow>>…` lines),
   - fake `kitty`/`kitten` scripts.
   Then: spawn → assert exec_cmd dispatched with `drove-<id>` → write openwindow → run `drove hook claude` with fixture stdin + `DROVE_AGENT_ID` → assert `ls --json` statuses → activewindowv2 clears attention → closewindow → Exited.
3. A real headless Hyprland + kitty environment via Nix is being prepared separately (`flake.nix` / `tests/headless/`); don't block on it.

## Conventions
- `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test` must pass before every commit.
- Small commits, pushed to `claude/intelligent-wozniak-1pb9fe` as you go.
- Dependencies: keep lean — tokio, serde, serde_json, clap (derive), toml, anyhow, thiserror (optional), tracing + tracing-subscriber. No chrono, no reqwest.
- README.md: what it is, install, config, hyprland.lua snippet, hooks setup per agent, protocol for UI authors, **manual smoke-test checklist for the user to run on their real machine**.
