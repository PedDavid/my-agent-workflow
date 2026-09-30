# Verifies the Hyprland ≥ 0.55 Lua-config IPC surface drove depends on.
{ pkgs }:
pkgs.testers.runNixOSTest {
  name = "vm-hyprland";
  nodes.machine = { ... }: { imports = [ (import ./common.nix { inherit pkgs; }) ]; };
  testScript = builtins.readFile ./test-lib.py + ''

    wait_hyprland()
    print(hc("monitors"))

    # Lua-mode probe used by `hyprland.dispatch = "auto"`.
    assert hc("dispatch", "hl.dsp.no_op()").strip() == "ok"

    # exec_cmd with a rule table; kitty --class becomes the Hyprland class.
    assert hc("dispatch", 'hl.dsp.exec_cmd("kitty --class probe", { workspace = "2" })').strip() == "ok"
    machine.wait_until_succeeds("su alice -c 'hc -j clients' | grep -q '\"probe\"'", timeout=600)
    probe = next(c for c in clients() if c["class"] == "probe")
    assert probe["workspace"]["name"] == "2", probe
    assert probe["address"].startswith("0x"), probe

    # focus by address selector
    assert hc("dispatch", 'hl.dsp.focus({ window = "address:%s" })' % probe["address"]).strip() == "ok"
    machine.wait_until_succeeds("su alice -c 'hc -j activewindow' | grep -q '\"probe\"'", timeout=60)

    # focus by class selector
    assert hc("dispatch", 'hl.dsp.exec_cmd("kitty --class probe2")').strip() == "ok"
    machine.wait_until_succeeds("su alice -c 'hc -j activewindow' | grep -q '\"probe2\"'", timeout=600)
    assert hc("dispatch", 'hl.dsp.focus({ window = "class:^probe$" })').strip() == "ok"
    machine.wait_until_succeeds("su alice -c 'hc -j activewindow' | grep -q '\"probe\"'", timeout=60)

    # socket2 event format (openwindow/closewindow/activewindowv2)
    machine.succeed("su alice -c 'as-alice bash -c \"timeout 120 socat -u UNIX-CONNECT:\\$XDG_RUNTIME_DIR/hypr/\\$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock - > /tmp/s2.log 2>&1 &\"'")
    machine.sleep(2)
    assert hc("dispatch", 'hl.dsp.exec_cmd("kitty --class probe3 -o confirm_os_window_close=0")').strip() == "ok"
    machine.wait_until_succeeds("grep -q '^openwindow>>[0-9a-f]*,[^,]*,probe3,' /tmp/s2.log", timeout=600)
    machine.wait_until_succeeds("grep -q '^activewindowv2>>[0-9a-f]' /tmp/s2.log", timeout=60)
    assert hc("dispatch", 'hl.dsp.window.close({ window = "class:^probe3$" })').strip() == "ok"
    try:
        machine.wait_until_succeeds("grep -q '^closewindow>>[0-9a-f]' /tmp/s2.log", timeout=60)
    except Exception:
        print(machine.succeed("cat /tmp/s2.log"))
        print(hc("-j", "clients"))
        raise
    assert not any(c["class"] == "probe3" for c in clients())
    print(machine.succeed("cat /tmp/s2.log"))

    # kitty remote control over a per-instance socket (drove's standalone mode)
    assert hc("dispatch", 'hl.dsp.exec_cmd("kitty --class rc --listen-on unix:/tmp/kitty-rc.sock -o allow_remote_control=socket-only")').strip() == "ok"
    machine.wait_until_succeeds("test -S /tmp/kitty-rc.sock", timeout=600)
    ls = json.loads(alice("kitten @ --to unix:/tmp/kitty-rc.sock ls"))
    assert ls[0]["tabs"][0]["windows"][0]["id"] >= 1, ls
    alice("kitten @ --to unix:/tmp/kitty-rc.sock send-text 'echo drove-marker\\r'")
    machine.wait_until_succeeds("su alice -c 'kitten @ --to unix:/tmp/kitty-rc.sock get-text --extent all' | grep -q '^drove-marker'", timeout=120)

    machine.screenshot("hyprland")
  '';
}
