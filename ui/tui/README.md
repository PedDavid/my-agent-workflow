# drove-tui

Terminal dashboard for the drove daemon (ratatui + crossterm). It talks to the
daemon only over the socket protocol, through the shared `drove-client` crate.

```sh
cd ui
cargo run -p drove-tui -- [--socket PATH]
```

The socket defaults to `drove_client::socket_path()` (`$DROVE_SOCKET`, else
`$XDG_RUNTIME_DIR/drove/drove.sock`). `--socket` overrides it.

## Try it against the mock

```sh
python3 tools/mock-drove.py --socket /tmp/drove-mock.sock &
cd ui && cargo run -p drove-tui -- --socket /tmp/drove-mock.sock
```

## Keys

| key | action |
|---|---|
| `j`/`k`, `↑`/`↓` (`g`/`G`) | move selection (first / last) |
| `Enter` | focus the agent's Hyprland window |
| `n` | focus the next agent that needs you |
| `s` | send a prompt: type, `Enter` sends (with enter), `Esc` cancels |
| `r` | rename (input pre-filled with the current name) |
| `x` | close the window, asks `y/n` |
| `f` | forget all exited agents |
| `?` | help overlay |
| `q`, `Ctrl-C` | quit |

## Screen

```
 drove    ! 1 needs input  ⠹ 1 working  ● 2 idle  ○ 0 starting  × 0 exited
────── agents ──────────────────────────────────────────────────────────────
▶ ! fix-flaky-tests  codex  ws2  ~/src/fix-flaky-tests  Allow this action? [y/n/t]
▶ ● infra-docs       kiro   ws3  ~/src/infra-docs       done: summarized the repo (guess)
  ⠹ api-refactor     claude ws1  ~/src/api-refactor     Bash: cargo test
  ● scratch          claude ws3  ~/src/scratch
────── api-refactor · ab12cd ───────────────────────────────────────────────
claude — api-refactor

> Bash: cargo test

[working]
 renamed to api
 j/k move  Enter focus  n next  s send  r rename  x close  f forget  ? help  q quit
```

`▶` marks agents that want attention. Status colours: starting grey, idle
green, working cyan spinner, needs_input bold red `!`, exited dim `×`. The
preview shows `get_text(id, "screen")` of the selected agent, refreshed every
~2 s (and immediately on selection change) from a separate thread. Daemon
errors show in the status line above the key hints. When the daemon is
unreachable a red banner appears and the follower keeps reconnecting with
backoff.

## Layout of the code

- `src/app.rs` pure state, `handle_key(KeyEvent) -> Vec<Action>`, `handle_msg`.
- `src/ui.rs` rendering (tested with ratatui `TestBackend`).
- `src/worker.rs` follower / executor / preview threads, `run_action`.
- `tests/mock_daemon.rs` starts `tools/mock-drove.py`, drives the app headlessly
  and asserts the mock's request log.

## Decisions

- `worker::follow_at` is a small local copy of `drove_client::follow` that takes
  an explicit socket path (and reports successful connects) so tests do not
  need the process-global `DROVE_SOCKET`. `--socket` still also sets
  `DROVE_SOCKET` for the process as requested.
- Actions run on an executor thread with its own persistent connection
  (reconnects once if it went stale); the UI thread never blocks on the daemon.
- `send` always uses `enter=true`. `f` forgets every exited agent, not just the
  selected one.
