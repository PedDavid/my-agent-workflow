# Quickshell integration test: real Hyprland + quickshell running
# ui/quickshell/shell.qml against tools/mock-drove.py on the default socket
# path. Asserts the service subscribes, builds the model, and that actions
# (next / focus / close / send) reach the daemon. Screenshot goes to $out.
{ pkgs }:
let
  shellDir = ../ui/quickshell;
  mock = ../tools/mock-drove.py;
in
pkgs.testers.runNixOSTest {
  name = "vm-quickshell";
  nodes.machine = { lib, ... }: {
    imports = [ (import ./common.nix { inherit pkgs; extraPackages = [ pkgs.quickshell pkgs.python3 ]; }) ];
    fonts.packages = [ pkgs.dejavu_fonts ];
    # Quickshell/Qt on llvmpipe wants a bit more room than the bare compositor.
    virtualisation.memorySize = lib.mkForce 3072;
  };
  testScript = builtins.readFile ./test-lib.py + ''

    SHELL = "${shellDir}/shell.qml"
    MOCK = "${mock}"
    SOCK = "/run/user/1000/drove/drove.sock"
    LOG = "/tmp/mock-requests.log"


    def qs(*args: str) -> str:
        cmd = "quickshell ipc -p " + shlex.quote(SHELL) + " " + " ".join(shlex.quote(a) for a in args)
        return alice(cmd).strip()


    def requests() -> list:
        out = machine.succeed("cat " + LOG + " 2>/dev/null || true")
        return [json.loads(l) for l in out.splitlines() if l.strip()]


    def state() -> list:
        return qs("call", "drove", "state").split()


    def safe_state() -> list:
        try:
            return state() or ["?"]
        except Exception:
            return ["?"]


    def qs_alive() -> None:
        machine.succeed("su alice -c 'kill -0 $(cat /tmp/qs.pid)'")


    wait_hyprland()

    # Mock daemon on the default socket path, frozen so states are deterministic:
    # api-refactor working, fix-flaky-tests needs_input(+attention),
    # infra-docs idle+attention, scratch idle.
    alice("mkdir -p /run/user/1000/drove")
    alice(f"setsid python3 {MOCK} --socket {SOCK} --tick 0 --log {LOG} > /tmp/mock.out 2>&1 < /dev/null & echo $! > /tmp/mock.pid")
    machine.wait_until_succeeds("test -S " + SOCK, timeout=120)

    # Start quickshell with the demo shell.
    alice(f"setsid quickshell -p {SHELL} > /tmp/qs.log 2>&1 < /dev/null & echo $! > /tmp/qs.pid")
    try:
        machine.wait_until_succeeds("grep -q '\"method\": *\"subscribe\"' " + LOG, timeout=600)
        qs_alive()

        # Model + counts arrived: connected needsInput attention working idle total first
        def model_ready(_=None):
            try:
                return state() == ["true", "1", "1", "1", "1", "4", "fix-flaky-tests"]
            except Exception:
                return False
        retry(model_ready, 300)

        # Still alive after a settle period, no QML errors.
        machine.sleep(5)
        qs_alive()
        qlog = machine.succeed("cat /tmp/qs.log")
        print(qlog)
        assert "ERROR" not in qlog and "Failed to load" not in qlog, qlog

        # Actions over the request connection.
        qs("call", "drove", "next")
        machine.wait_until_succeeds("grep -q '\"method\": *\"next\"' " + LOG, timeout=60)
        qs("call", "drove", "focus", "api-refactor")
        machine.wait_until_succeeds("grep -q '\"method\": *\"focus\"' " + LOG, timeout=60)
        qs("call", "drove", "send", "scratch", "hello agent")
        machine.wait_until_succeeds("grep -q '\"method\": *\"send_text\"' " + LOG, timeout=60)
        qs("call", "drove", "close", "api-refactor")
        machine.wait_until_succeeds("grep -q '\"method\": *\"close\"' " + LOG, timeout=60)

        reqs = requests()
        by = {r["method"]: r for r in reqs}
        assert by["focus"]["params"] == {"id": "api-refactor"}, reqs
        assert by["close"]["params"] == {"id": "api-refactor"}, reqs
        assert by["send_text"]["params"] == {"id": "scratch", "text": "hello agent", "enter": True}, reqs
        # actions must not ride on the subscribed connection: exactly one subscribe
        assert sum(1 for r in reqs if r["method"] == "subscribe") == 1, reqs

        # The close event flows back through the subscription: api-refactor no longer working (scratch became working via send_text).
        def closed(_=None):
            return state()[:6] == ["true", "1", "1", "1", "0", "4"]
        retry(closed, 120)

        # Open the popup panel and capture the result.
        qs("call", "drove", "togglePanel")
        machine.sleep(10)
        machine.screenshot("quickshell")
    except Exception:
        print(machine.execute("cat /tmp/qs.log; cat /tmp/mock.out; cat " + LOG)[1])
        machine.screenshot("quickshell-fail")
        raise

    # Daemon restart: the service must reconnect with backoff and resubscribe.
    try:
        machine.succeed("kill $(cat /tmp/mock.pid)")
        retry(lambda _=None: safe_state()[0] == "false", 60)
        alice(f"setsid python3 {MOCK} --socket {SOCK} --tick 0 --log {LOG} > /tmp/mock2.out 2>&1 < /dev/null & echo $! > /tmp/mock.pid")
        retry(lambda _=None: safe_state()[:6] == ["true", "1", "1", "1", "1", "4"], 120)
        qs_alive()
    except Exception:
        print(machine.execute("cat /tmp/qs.log; cat /tmp/mock2.out")[1])
        print(safe_state())
        machine.screenshot("quickshell-fail")
        raise
  '';
}
