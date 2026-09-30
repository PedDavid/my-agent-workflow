# drove-notify

Desktop notifications (freedesktop `org.freedesktop.Notifications`, via
`notify-rust` + zbus, no libdbus) for [drove](../../README.md) agents. It only
talks to the daemon through the socket protocol (`docs/protocol.md`).

```
drove-notify [--socket PATH] [--config PATH] [--dry-run]
```

- `--socket` daemon socket (default `$DROVE_SOCKET` / standard path)
- `--config` TOML file (default `~/.config/drove/notify.toml`, optional)
- `--dry-run` print what would be sent as JSON lines instead of using D-Bus.
  Lines on stdin (`focus ID`, `dismiss ID`) simulate clicking a button.

## Behaviour

State is kept per agent; every `agent` event is diffed against the previous one.
Snapshots (the first one and any re-snapshot after a reconnect) only replace
state - they never notify.

| Change | Notification | Urgency | Category |
|---|---|---|---|
| any -> `needs_input` | `⚠ <name> needs you`, body = detail (+ ` (guess)` if `maybe`) | critical (never expires) | `x-drove.needs-input` |
| `working` -> `idle` with `attention = true` | `✓ <name> finished`, body = detail | normal | `x-drove.finished` |
| any -> `exited` (unless we asked for the close) | `× <name> exited` | low | `x-drove.exited` |
| `working` -> `idle` with `attention = false` | nothing (you were looking) | | |
| agent goes `working` again, `attention` cleared (focused elsewhere), needs_input answered, agent forgotten | closes that agent's notification | | |

- One notification slot per agent: a new one *replaces* the old one (same
  notification id) rather than stacking.
- Actions: default click and the **Focus** button call `focus` on the daemon
  (and close the notification); **Dismiss** just closes it.
- Rate limit: at most one notification per agent per `rate_limit_secs`
  (default 10). Suppressed events do not restart the window.
- Quiet mode: `quiet = true` in the config, or the file
  `$XDG_RUNTIME_DIR/drove/quiet` exists (checked on every event, so
  `touch`/`rm` toggles it live). Quiet suppresses notifications and sounds;
  closing stale notifications still happens.
- App name `drove`, hint `desktop-entry = drove`, configurable icon.
- `needs_input` is notified even when `attention = false`; only the idle
  "finished" case is gated on attention.
- If the daemon is down the notifier retries with backoff; the snapshot after
  reconnecting is silent.

## Config reference

```toml
quiet = false              # global quiet mode
rate_limit_secs = 10       # per agent; 0 disables
icon = "utilities-terminal"  # icon name or absolute path
timeout_ms = 0             # expiry for non-critical notifications; 0 = server default

[transitions]              # which transitions notify (all default true)
needs_input = true
finished = true
exited = true

[sound]                    # shell command run with `sh -c` when that notification fires
needs_input = "pw-play /usr/share/sounds/freedesktop/stereo/dialog-warning.oga"
finished = "pw-play /usr/share/sounds/freedesktop/stereo/complete.oga"
exited = ""
```

Sound commands get `DROVE_AGENT_ID` in their environment and are not run when
the notification is suppressed (quiet, rate limit, transition disabled).

## Dry-run output

```json
{"op":"notify","app_name":"drove","agent":"ab12cd","name":"api","transition":"needs_input","summary":"⚠ api needs you","body":"...","urgency":"critical","category":"x-drove.needs-input","icon":"utilities-terminal","desktop_entry":"drove","actions":["Focus","Dismiss"]}
{"op":"close","agent":"ab12cd"}
{"op":"sound","agent":"ab12cd","command":"pw-play ..."}
```

## Running it

systemd user unit (`~/.config/systemd/user/drove-notify.service`):

```ini
[Unit]
Description=drove desktop notifications
After=graphical-session.target
PartOf=graphical-session.target

[Service]
ExecStart=%h/.cargo/bin/drove-notify
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
```

Hyprland (Lua config):

```lua
hl.exec_cmd("drove-notify")
```

## Tests

`cargo test -p drove-notify` from `ui/`: unit tests (policy, engine, config),
dry-run integration tests against `tools/mock-drove.py` (needs `python3`), and
a real D-Bus test that starts a private `dbus-daemon` with a fake notification
server (skipped, with a message, when `dbus-daemon` is not installed).
