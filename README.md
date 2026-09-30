# drove

Keep track of coding agents with your window manager, not a multiplexer.

Every agent (Claude Code, Codex, kiro-cli, or any command) runs in its own **Hyprland
window**, inside its own kitty. Hyprland handles layout and kitty is only the terminal.
A small daemon follows each agent's state through the agent's own hooks and shares it
with a picker, a Waybar module, the CLI, and any UI you build on the event stream.

```
agent hooks ──► drove hook <src> ──(unix socket)──► drove daemon ◄── Hyprland socket2 events
                                                        │  ▲
                              hyprctl / kitten @ ◄──────┘  │ NDJSON request/response + subscribe
                                                           │
                        drove ls / pick / next / focus / spawn / bar / watch / your UI
```

Status per agent: `starting`, `idle`, `working`, `needs_input` or `exited`, plus an
**attention** flag. The flag is set when a turn finishes while you're looking at another
window, and cleared when you focus the agent's window.

## Install

```sh
cargo install --path .          # installs ~/.cargo/bin/drove
```

Requirements: Hyprland ≥ 0.55 with a Lua config (a legacy hyprlang config also works
with `dispatch = "legacy"`), kitty, and a dmenu-style picker (fuzzel by default) for
`drove pick`.

### Hyprland

Copy `contrib/hyprland.lua` to `~/.config/hypr/drove.lua` and `require("drove")` it
from `hyprland.lua`. It does three things:

* autostarts `drove daemon` from `hl.on("hyprland.start", …)`
* binds `SUPER+A` to pick, `SUPER+N` to next, `SUPER+SHIFT+A` to spawn claude, and ``SUPER+` `` to toggle the `special:agents` workspace
* includes a commented-out window rule for `class = "^drove-.*$"`

If you'd rather run the daemon from systemd, use `contrib/drove.service` and remove the
autostart line.

### Waybar

See `contrib/waybar.jsonc`. `drove bar` prints one JSON line on every change:
`{"text":"◉ 1  ◐ 2","tooltip":"…","class":["needs-input"],"alt":"needs-input"}`.
The class is one of `needs-input`, `working`, `idle` or `none`.

## Usage

```sh
drove spawn claude                          # new window in $PWD, prints the agent id
drove spawn codex --name api --cwd ~/src/api
drove spawn claude --worktree feat/login    # git worktree at ../<repo>.worktrees/feat-login
drove spawn kiro --workspace "3 silent" -- --agent dev   # args after -- go to the agent
drove ls                  # table (add --all for exited agents, --json for raw)
drove next                # focus: needs input > finished turn > anything flagged
drove pick                # fuzzel list; the chosen agent is focused
drove focus api | drove close api | drove rename api web
drove send api --enter "run the tests"      # type into the agent's kitty
drove text api [--extent all]               # dump the agent's screen
drove forget api | drove forget --exited
drove watch               # raw event stream
drove status              # is the daemon up, which Hyprland dispatch mode
```

Wherever an agent is expected you can give its id, its name, or a unique prefix of its id.

Icons: `◌` starting, `○` idle, `●` idle with attention, `◐` working, `◉` needs input,
`✕` exited. A `?` after the status marks a guess (the kiro heuristic, see below).

## Configuration

`~/.config/drove/config.toml`. Every key is optional. `contrib/config.toml` lists all
keys with their defaults. The ones you're most likely to change:

```toml
[spawn]
workspace = "special:agents"   # Hyprland workspace rule for new agent windows

[picker]
command = ["wofi", "--dmenu"]

[agents.opus]                  # your own profile: `drove spawn opus`
kind = "claude"
command = ["claude", "--model", "opus"]
```

Built-in profiles: `claude`, `codex`, `kiro` (`kiro-cli chat`) and `shell` (`$SHELL`,
no hooks).

Environment overrides, mostly for tests: `DROVE_CONFIG`, `DROVE_SOCKET`, `DROVE_STATE_DIR`,
`DROVE_DATA_DIR`, `DROVE_LOG` (e.g. `debug`), and `DROVE_DEBUG=1` (makes `drove hook`
report errors on stderr).

Files:

* `$XDG_RUNTIME_DIR/drove/drove.sock`: daemon socket
* `$XDG_RUNTIME_DIR/drove/kitty-<id>.sock`: per-agent kitty remote-control socket
* `$XDG_STATE_HOME/drove/state.json`: agents, rewritten atomically on every change
* `$XDG_DATA_HOME/drove/claude-settings.json`: generated hook settings for Claude

## Hook setup per agent

`drove hook …` never writes to stdout, because for some events the agent adds stdout to
the model's context. It always exits 0, and it gives up after about 200 ms when the daemon
isn't running.

### Claude Code: nothing to do

`drove spawn` starts `claude --settings $XDG_DATA_HOME/drove/claude-settings.json …`.
Claude layers that file on top of your own settings, and your settings files are left
alone. Only if you also want to track Claude sessions you start **by hand**, run
`drove hooks install claude`, which merges the hooks into `~/.claude/settings.json`.
Print the snippet with `drove hooks print claude`. Claude runs identical hook commands only once, so installing
them doesn't double-fire for sessions that drove spawned.

### Codex

* **Always works:** spawned Codex gets `-c 'notify=["<drove>","hook","codex-notify"]'`,
  so every finished turn marks the agent idle with attention. This replaces any `notify`
  in your own config; set `inject = false` on the profile to turn it off.
* **Full status** (working, tool, permission): run `drove hooks install codex`. It merges
  drove's entries into `~/.codex/hooks.json` (or `$CODEX_HOME/hooks.json`). It's
  idempotent, and the old file is kept as `hooks.json.drove-bak`. Then **start codex once
  and trust the new hooks with `/hooks`**.

### kiro-cli

Hooks live in the agent config. `drove hooks install kiro` updates every
`~/.kiro/agents/*.json`, and `--agent dev` updates or creates only `dev.json`. To paste
by hand, use `drove hooks print kiro`.

kiro has no hook for "waiting for permission". drove guesses instead: if a `preToolUse`
gets no `postToolUse` within `kiro.stale_tool_secs` (4 s by default), the agent is shown as
`needs input?`.

### Agents started outside drove (adoption)

When a hook arrives without `DROVE_AGENT_ID`, it sends its ancestor PIDs. If exactly one
Hyprland window belongs to one of those PIDs, the daemon adopts that window as a new agent
(`adopted: true`). Terminals that run one process per window (foot, alacritty, plain `kitty`)
work. A shared kitty instance with several windows is ambiguous, so it's skipped. Turn
this off with `[hooks] adopt = false`.

## Event-stream protocol (for Quickshell and other UIs)

Connect to `$XDG_RUNTIME_DIR/drove/drove.sock` (a unix stream socket). The protocol is
newline-delimited JSON.

Request/response:

```json
→ {"id":1,"method":"list","params":{}}
← {"id":1,"ok":true,"result":[Agent…]}
← {"id":2,"ok":false,"error":"no agent \"x\""}
```

| method | params | result |
|---|---|---|
| `ping` | – | `"pong"` |
| `status` | – | `{version,pid,socket,state,hyprland,dispatch,agents}` |
| `list` | – | `[Agent]` |
| `get` | `{id}` | `Agent` |
| `spawn` | `{profile, name?, cwd?, worktree?, workspace?, args?}` | `Agent` |
| `focus` | `{id}` | `Agent` |
| `next` | – | `Agent` or `null` |
| `close` | `{id}` | `null` |
| `send_text` | `{id, text, enter?}` | `null` |
| `get_text` | `{id, extent?}` (`screen`/`all`/`last_cmd_output`) | string |
| `rename` | `{id, name}` | `null` |
| `forget` | `{id}` or `{exited:true}` | number removed |
| `hook` | `{agent_id?, source, payload, pids?}` | `null` |
| `subscribe` | – | `null`, then the stream below |
| `shutdown` | – | `null` |

`id` in params can be an agent id, a name, or a unique id prefix.

**Subscribe.** After the `ok` response, the connection carries only events, one per line:

```json
{"event":"snapshot","agents":[Agent…]}      // first, and again if you fall behind
{"event":"agent","agent":Agent}             // added or changed: full object, replace yours
{"event":"removed","id":"k3xq7a"}
```

Agent object:

```json
{
  "id": "k3xq7a", "name": "claude-api", "kind": "claude", "profile": "claude",
  "cwd": "/home/me/src/api", "status": "needs_input", "attention": true,
  "detail": "permission: Bash", "maybe": false, "session_id": "…",
  "window": {"address": "0x55d1c0ffee", "workspace": "3", "title": "…", "class": "drove-k3xq7a"},
  "term": {"type": "kitty", "socket": "unix:/run/user/1000/drove/kitty-k3xq7a.sock"},
  "worktree": null, "adopted": false,
  "created_at": 1790000000000, "updated_at": 1790000000000, "last_hook_at": 1790000000000
}
```

`status` is one of `starting`, `idle`, `working`, `needs_input` or `exited`. Timestamps are
unix milliseconds. `maybe` is present only when true. Treat unknown fields as optional.
New ones may be added.

A Quickshell widget needs only `socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/drove/drove.sock`
(or a `Socket`) to send `{"id":1,"method":"subscribe"}`, then fold the events into a
map keyed by id.

## Development

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

`tests/e2e.rs` runs the real daemon against fake `hyprctl`, `kitty` and `kitten` shell
scripts and a fake socket2 served by the test. It covers spawn → openwindow → hooks → ls →
next/focus → attention → send/text → codex notify → the kiro heuristic → adoption →
close → exited → restart and reconcile.

## Manual smoke-test checklist (real Hyprland machine)

Run these on your real Hyprland machine. Set `DROVE_LOG=debug` on the daemon if something
looks wrong.

1. [ ] `cargo install --path .`, then `drove daemon` in a terminal. It should log
       `hyprland dispatch mode: lua`. Then `drove status` in another terminal should
       print `hyprland: yes (dispatch: lua)`.
2. [ ] `hyprctl dispatch 'hl.dsp.no_op()'` prints `ok`. This is the probe drove uses.
3. [ ] `drove spawn shell` opens a kitty window. `hyprctl clients` shows class
       `drove-<id>`, and `drove ls` shows it `idle` with a workspace.
4. [ ] `drove spawn shell --workspace "special:agents"` opens on the special workspace,
       which confirms the `exec_cmd` rule table works.
5. [ ] Focus another window, then `drove focus <id>`. Focus moves to the agent.
6. [ ] `drove close <id>`. The window closes and `drove ls --all` shows it `exited`.
       **If close doesn't work**, `hl.dsp.window.close({ window = … })` has a different
       signature. drove then falls back to focus + `hl.dsp.window.close()`, so report
       which of the two happened.
7. [ ] `drove spawn claude`. `drove ls` goes `starting` → `idle` after the Claude
       SessionStart hook. Send a prompt: `working` with the prompt snippet, then the tool
       name. On a permission prompt: `needs input !`. When it finishes while another
       window is focused: `idle !` (`●`). Focusing the window clears the `!`.
8. [ ] `drove send <id> --enter "say hi"` types into Claude. `drove text <id>` prints its
       screen.
9. [ ] Codex: `drove spawn codex`. After a turn it shows `idle !` from notify. Run
       `drove hooks install codex`, trust with `/hooks` in codex, spawn again, and check
       that `working` and `needs input` show up too.
10. [ ] kiro: `drove hooks install kiro --agent <yours>`, then
        `drove spawn kiro -- --agent <yours>`. You should see `working` during tools, and
        `needs input?` on a permission prompt after about 4 s.
11. [ ] `SUPER+A` (pick) and `SUPER+N` (next) from `contrib/hyprland.lua` work. Waybar
        `custom/drove` updates live.
12. [ ] Kill and restart the daemon. Agents keep their state, and windows are re-bound
        after the reconcile.
13. [ ] Adoption: run `claude` by hand in a foot or alacritty window after
        `drove hooks install claude`. It shows up in `drove ls` as adopted.
14. [ ] `drove spawn claude --worktree test/drove` inside a git repo creates
        `../<repo>.worktrees/test-drove` and starts there.
15. [ ] Legacy fallback (optional): with a hyprlang config, `dispatch = "auto"` should
        detect `legacy`. Spawn, focus and close should still work.

## Design notes / open questions

* **Branch.** The spec says to push to `claude/intelligent-wozniak-1pb9fe`, but the
  session had `claude/magical-volta-5t7key` checked out (the earlier WIP commit is
  there), so that's where the work went.
* **Close under Lua.** The `{ window = … }` selector for `hl.dsp.window.close` is
  unconfirmed; the upstream example calls `hl.dsp.window.close()` with no arguments.
  drove tries the selector form first, then falls back to focus + close. See checklist
  item 6.
* **Output of `hyprctl dispatch`.** drove treats a trimmed `ok` as success in both modes.
  If Lua dispatches print something else on success, the probe falls back to legacy
  mode. You can force the mode with `dispatch = "lua"`.
* **Instance mode** (`terminal.mode = "instance"`) launches through `kitten @ launch`
  instead of Hyprland, so the workspace rule isn't applied. Use a window rule on
  `class = "^drove-.*$"` instead. It's untested against a real kitty.
* **Claude `Notification`.** `idle_prompt` counts as idle without attention. Any other
  `notification_type` except `auth_success` counts as needs input. A notification with
  no type falls back to matching the message text "waiting for your input".
* **`SubagentStop`/`SubagentStart`/`PostCompact` are ignored.** A subagent finishing
  doesn't end the parent's turn. Codex `Interrupt` counts as the end of a turn.
* **`SessionStart` while working** (Claude's auto-compact fires it in the middle of a
  turn) doesn't change the status.
* **`NeedsInput` always sets attention**, even when the window is focused, as the plan
  says. Answering the prompt (`PostToolUse`) clears it.
* **An unknown `DROVE_AGENT_ID`** in a hook (for example after state.json was deleted)
  recreates a minimal adopted agent. Unknown `drove-*` windows seen in events or during
  reconcile are adopted too.
* **Reconcile** runs at startup and on every socket2 reconnect. An agent still
  `starting` within 60 s of its spawn isn't marked exited, so a slow window start
  survives it.
* **PATH.** Agent windows are started by Hyprland (`exec_cmd`), so `claude`, `codex`
  and `kiro-cli` must be on *Hyprland's* PATH. If they aren't, put absolute paths in the
  profiles' `command`.
* **Codex without `hooks.json`** stays `starting` until its first turn finishes, because
  notify only reports finished turns.
* **Not under Hyprland**, spawn falls back to a detached `setsid` process, and focus and
  close report an error.
* **Seams kept for later:** agent identity is `DROVE_AGENT_ID`, never the window.
  `TermRef` is a tagged enum. The WM layer offers only
  clients/focus/close/exec/events.
