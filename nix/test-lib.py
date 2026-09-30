# Helpers shared by the VM test scripts (prepended to each testScript).
import json
import shlex


def alice(cmd: str, succeed: bool = True) -> str:
    """Run a shell command as alice inside the Hyprland session env."""
    full = "su alice -c " + shlex.quote("as-alice bash -c " + shlex.quote(cmd))
    if succeed:
        return machine.succeed(full)
    return machine.execute(full)[1]


def hc(*args: str) -> str:
    # Under TCG emulation Hyprland's IPC occasionally times out while it is
    # busy (exit 6, "didn't respond in time"); retry those.
    cmd = "hc " + " ".join(shlex.quote(a) for a in args)
    for attempt in range(5):
        status, out = machine.execute("su alice -c " + shlex.quote("as-alice bash -c " + shlex.quote(cmd)))
        if status == 0:
            return out
        if "respond in time" not in out and "Couldn't read" not in out:
            break
        machine.sleep(2)
    raise AssertionError(f"hc {args} failed ({status}): {out}")


def clients() -> list:
    return json.loads(hc("-j", "clients"))


def active_class() -> str:
    out = hc("-j", "activewindow").strip()
    return json.loads(out).get("class", "") if out.startswith("{") else ""


def wait_hyprland() -> None:
    machine.wait_for_unit("multi-user.target")
    machine.wait_until_succeeds("su alice -c 'hc monitors' | grep -q Virtual", timeout=900)


def dump_logs() -> None:
    print(machine.execute("cat /tmp/hyprland.out | tail -50; tail -80 /run/user/1000/hypr/*/hyprland.log; ls -la /run/user/1000/drove 2>&1; cat /tmp/drove*.log 2>&1 | tail -100")[1])
