# VM tests: real Hyprland (Lua config) + kitty

Two NixOS VM checks boot QEMU with `virtio-gpu` (mesa llvmpipe renders, so no
host GPU is needed), autologin a user, and start Hyprland with a Lua config.

| check | what it proves |
|---|---|
| `vm-hyprland` | the Hyprland ≥ 0.55 Lua IPC + socket2 + kitty remote-control behaviour drove relies on |
| `vm-drove` | drove end to end: daemon under Hyprland, spawn → window mapping, Claude/Codex/kiro hooks → states, attention, `next`/`focus`, `send`/`text`, restart reconcile, close, worktrees |

```sh
nix build -L .#checks.x86_64-linux.vm-hyprland
nix build -L .#checks.x86_64-linux.vm-drove
nix flake check -L          # both
```

With KVM each takes a minute or two. Without KVM (e.g. inside a container)
QEMU falls back to TCG: ~5 min for `vm-hyprland`. In that case add
`system-features = nixos-test benchmark big-parallel kvm` to `nix.conf` so Nix
agrees to run the test, and expect slow IPC — the test helpers retry
"didn't respond in time".

Interactive debugging:

```sh
nix run .#checks.x86_64-linux.vm-drove.driverInteractive
>>> start_all(); wait_hyprland(); machine.screenshot("x")
```

## Fake agents (`fake-agents.nix`)

`claude`, `codex` and `kiro-cli` stand-ins that find their hook commands the
same way the real tools do (`--settings` file; `-c notify=[…]` +
`~/.codex/hooks.json`; `~/.kiro/agents/*.json`) and fire realistic payloads.
They read lines from the terminal (so `drove send` drives them):

- any text → prompt → tool start → tool end → turn done
- `perm` → permission request (claude) / never-finishing tool (kiro); next line resolves it
- `exit` → session end

They also flag any hook that writes to stdout (real agents inject that into
the model context), and the test fails on it.
