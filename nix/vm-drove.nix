# End-to-end acceptance test for drove, written against the CLI in PLAN.md.
# Real Hyprland (Lua config) + real kitty; fake agents fire real hooks.
{ pkgs, drove, fakeAgents }:
pkgs.testers.runNixOSTest {
  name = "vm-drove";
  nodes.machine = { ... }: {
    imports = [
      (import ./common.nix {
        inherit pkgs;
        extraPackages = [ drove ] ++ fakeAgents.all;
      })
    ];
  };

  testScript = builtins.readFile ./test-lib.py + ''

    import time


    def drove(*args: str, succeed: bool = True) -> str:
        return alice("drove " + " ".join(shlex.quote(a) for a in args), succeed=succeed)


    def agents() -> list:
        data = json.loads(drove("ls", "--json"))
        return data["agents"] if isinstance(data, dict) else data


    def agent(aid: str) -> dict:
        for a in agents():
            if a["id"] == aid:
                return a
        raise AssertionError(f"agent {aid} not in ls: {agents()}")


    def status(aid: str) -> str:
        return str(agent(aid)["status"]).lower().replace("_", "")


    def wait_for(pred, what: str, timeout: int = 180):
        deadline = time.time() + timeout
        last = None
        while time.time() < deadline:
            try:
                last = pred()
                if last:
                    return last
            except Exception as e:  # daemon may not be up yet, etc.
                last = e
            machine.sleep(1)
        dump_logs()
        print(machine.execute("cat /tmp/fake-agents.log")[1])
        raise AssertionError(f"timed out waiting for {what}; last={last!r}; agents={drove('ls', '--json', succeed=False)}")


    def wait_status(aid: str, want: str, timeout: int = 180):
        wait_for(lambda: status(aid) == want, f"{aid} status={want}", timeout)


    def window_of(aid: str):
        return next((c for c in clients() if c["class"] == f"drove-{aid}"), None)


    def spawn(profile: str, name: str, *extra: str) -> str:
        out = drove("spawn", profile, "--name", name, "--cwd", "/home/alice/proj", *extra).strip()
        aid = out.split()[-1]
        wait_for(lambda: window_of(aid), f"hyprland window drove-{aid}", 600)
        return aid


    def focus_elsewhere():
        assert hc("dispatch", 'hl.dsp.focus({ window = "class:^scratch$" })').strip() == "ok"
        wait_for(lambda: active_class() == "scratch", "scratch focused", 60)


    wait_hyprland()
    machine.succeed("su alice -c 'mkdir -p ~/proj && cd ~/proj && git init -q && git -c user.email=a@b -c user.name=a commit -q --allow-empty -m init'")

    # A plain window we can move focus to, so agents are "in the background".
    hc("dispatch", 'hl.dsp.exec_cmd("kitty --class scratch")')
    wait_for(lambda: any(c["class"] == "scratch" for c in clients()), "scratch window", 600)

    with subtest("daemon starts under Hyprland and reports lua dispatch"):
        hc("dispatch", 'hl.dsp.exec_cmd("drove daemon > /tmp/drove.log 2>&1")')
        wait_for(lambda: "lua" in drove("status").lower(), "drove status mentions lua", 120)

    with subtest("claude: spawn → idle → working → idle+attention → focus clears"):
        c1 = spawn("claude", "c1")
        wait_status(c1, "idle")                       # SessionStart hook
        assert agent(c1)["window"], agent(c1)
        focus_elsewhere()
        drove("send", c1, "summarize the repo", "--enter")
        wait_status(c1, "working", 60)                # UserPromptSubmit / PreToolUse
        wait_status(c1, "idle", 60)                   # Stop
        assert agent(c1)["attention"] is True, agent(c1)
        drove("next")                                 # jumps to the agent needing attention
        wait_for(lambda: active_class() == f"drove-{c1}", "c1 focused by next", 60)
        wait_for(lambda: agent(c1)["attention"] is False, "attention cleared on focus", 30)

    with subtest("claude: permission request → needs_input"):
        focus_elsewhere()
        drove("send", c1, "perm", "--enter")
        wait_status(c1, "needsinput", 60)
        assert agent(c1)["attention"] is True, agent(c1)
        drove("focus", "c1")                          # by name
        wait_for(lambda: active_class() == f"drove-{c1}", "c1 focused by name", 60)
        drove("send", c1, "y", "--enter")
        wait_status(c1, "idle", 60)

    with subtest("hooks never write to agent context (stdout must be empty)"):
        machine.fail("grep -q HOOK-STDOUT-NOT-EMPTY /tmp/fake-agents.log")

    with subtest("codex: notify fallback alone drives turn completion"):
        x1 = spawn("codex", "x1")
        focus_elsewhere()
        drove("send", x1, "fix tests", "--enter")
        wait_for(lambda: agent(x1)["attention"] is True and status(x1) == "idle", "codex turn complete via notify", 90)

    with subtest("codex: installed hooks.json gives working state"):
        drove("hooks", "install", "codex")
        machine.succeed("su alice -c 'jq -e . ~/.codex/hooks.json'")
        drove("hooks", "install", "codex")            # idempotent
        n = alice("jq '[.. | objects | select(.command? // \"\" | test(\"drove\"))] | length' ~/.codex/hooks.json").strip()
        x2 = spawn("codex", "x2")
        drove("send", x2, "again", "--enter")
        wait_status(x2, "working", 60)
        wait_status(x2, "idle", 60)
        print("codex drove hook entries:", n)

    with subtest("kiro: installed agent hooks; stale preToolUse → needs_input"):
        machine.succeed("su alice -c 'mkdir -p ~/.kiro/agents && echo {\\\"name\\\":\\\"default\\\"} > ~/.kiro/agents/default.json'")
        drove("hooks", "install", "kiro")
        machine.succeed("su alice -c 'jq -e .hooks.stop ~/.kiro/agents/default.json'")
        k1 = spawn("kiro", "k1")
        wait_status(k1, "idle")                       # agentSpawn
        drove("send", k1, "hello", "--enter")
        wait_status(k1, "idle", 60)
        drove("send", k1, "perm", "--enter")
        wait_status(k1, "needsinput", 60)             # heuristic: tool started, never finished
        drove("send", k1, "y", "--enter")
        wait_status(k1, "idle", 60)

    with subtest("text: read an agent's screen via kitty"):
        assert "fake-claude ready" in drove("text", c1)

    with subtest("daemon restart reconciles live windows"):
        alice("pkill -f 'drove daemon' || true")
        machine.sleep(1)
        hc("dispatch", 'hl.dsp.exec_cmd("drove daemon > /tmp/drove2.log 2>&1")')
        wait_for(lambda: agent(c1)["window"] and status(c1) != "exited", "c1 re-attached after restart", 120)

    with subtest("close: window goes away and agent is exited"):
        drove("close", c1)
        wait_for(lambda: window_of(c1) is None, "c1 window closed", 60)
        wait_status(c1, "exited", 60)
        drove("forget", "--exited")
        assert all(a["id"] != c1 for a in agents())

    with subtest("agent exiting on its own marks it exited"):
        drove("send", x1, "exit", "--enter")
        wait_status(x1, "exited", 60)

    with subtest("worktree spawn"):
        w1 = spawn("claude", "w1", "--worktree", "feat-x")
        cwd = agent(w1)["cwd"]
        assert "feat-x" in cwd, cwd
        machine.succeed(f"su alice -c 'git -C {shlex.quote(cwd)} rev-parse --abbrev-ref HEAD' | grep -q feat-x")

    print(drove("ls"))
    machine.screenshot("drove")
  '';
}
