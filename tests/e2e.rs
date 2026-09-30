//! End to end: real daemon subprocess, fake hyprctl/kitty/kitten scripts and a
//! fake Hyprland socket2 served by the test.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_drove");

struct Env {
    dir: tempfile::TempDir,
    daemon: Option<Child>,
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Env {
    fn new(extra_config: &str) -> Self {
        // Short path: unix socket paths are limited to ~108 bytes.
        let dir = tempfile::Builder::new()
            .prefix("dv")
            .tempdir_in("/tmp")
            .unwrap();
        let d = dir.path();
        let log_args = |name: &str| {
            format!(
                "{{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; echo ---; }} >> {}/{name}.log\n",
                d.display()
            )
        };
        script(
            &d.join("hyprctl"),
            &format!(
                "{}if [ \"$1\" = -j ] && [ \"$2\" = clients ]; then cat {}/clients.json; else echo ok; fi\n",
                log_args("hyprctl"),
                d.display()
            ),
        );
        script(&d.join("kitty"), &log_args("kitty"));
        script(
            &d.join("kitten"),
            &format!(
                "{}case \"$*\" in *send-text*) cat >> {dir}/sent.txt ;; *get-text*) printf 'screen text' ;; esac\n",
                log_args("kitten"),
                dir = d.display()
            ),
        );
        std::fs::write(d.join("clients.json"), "[]").unwrap();
        std::fs::write(
            d.join("config.toml"),
            format!(
                r#"
[hyprland]
hyprctl = "{d}/hyprctl"
socket2 = "{d}/s2.sock"
[terminal]
kitty = "{d}/kitty"
kitten = "{d}/kitten"
[spawn]
workspace = "special:agents"
[kiro]
stale_tool_secs = 1
[hooks]
drove = "{BIN}"
{extra_config}
"#,
                d = d.display()
            ),
        )
        .unwrap();
        std::fs::create_dir(d.join("work")).unwrap();
        Env { dir, daemon: None }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let d = self.dir.path();
        let mut c = Command::new(BIN);
        c.args(args)
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .env_remove("DROVE_AGENT_ID")
            .env("XDG_RUNTIME_DIR", d.join("rt"))
            .env("DROVE_SOCKET", d.join("drove.sock"))
            .env("DROVE_CONFIG", d.join("config.toml"))
            .env("DROVE_STATE_DIR", d.join("state"))
            .env("DROVE_DATA_DIR", d.join("data"))
            .env("HOME", d);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "drove {args:?} failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    }

    fn hook(&self, source: &str, agent: Option<&str>, payload: &str) {
        let mut c = self.cmd(&["hook", source]);
        if let Some(a) = agent {
            c.env("DROVE_AGENT_ID", a);
        }
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success());
        assert!(o.stdout.is_empty(), "hooks must never write stdout");
    }

    fn start_daemon(&mut self) -> UnixStream {
        let s2 = self.path("s2.sock");
        let _ = std::fs::remove_file(&s2);
        let listener = UnixListener::bind(&s2).unwrap();
        listener.set_nonblocking(true).unwrap();
        let child = self
            .cmd(&["daemon"])
            .env("DROVE_LOG", "debug")
            .stderr(std::fs::File::create(self.path("daemon.log")).unwrap())
            .spawn()
            .unwrap();
        self.daemon = Some(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("daemon never connected to socket2: {e}"),
            }
        };
        wait_for(|| self.run(&["status"]).status.success(), "daemon status");
        stream
    }

    fn stop_daemon(&mut self) {
        let mut c = drove::client::Client::connect(&self.path("drove.sock"), None).unwrap();
        c.call("shutdown", Value::Null).unwrap();
        let mut d = self.daemon.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while d.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "daemon did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn agents(&self) -> Vec<Value> {
        let v: Value = serde_json::from_str(&self.ok(&["ls", "--json"])).unwrap();
        v.as_array().unwrap().clone()
    }

    fn agent(&self, id: &str) -> Value {
        self.agents()
            .into_iter()
            .find(|a| a["id"] == id)
            .unwrap_or_else(|| panic!("no agent {id}"))
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(&format!("{name}.log"))).unwrap_or_default()
    }
}

fn wait_for(mut f: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn event(s2: &mut UnixStream, line: &str) {
    s2.write_all(format!("{line}\n").as_bytes()).unwrap();
}

#[test]
fn full_lifecycle() {
    let mut env = Env::new("");
    let mut s2 = env.start_daemon();
    let status = env.ok(&["status"]);
    assert!(status.contains("hyprland: yes (dispatch: lua)"), "{status}");

    // spawn → exec_cmd through hyprctl with the drove-<id> class
    let work = env.path("work");
    let id = env
        .ok(&[
            "spawn",
            "claude",
            "--name",
            "api",
            "--cwd",
            work.to_str().unwrap(),
            "--",
            "--model",
            "x",
        ])
        .trim()
        .to_string();
    assert_eq!(id.len(), 6);
    let hl = env.log("hyprctl");
    let exec = hl
        .split("---\n")
        .find(|c| c.contains("hl.dsp.exec_cmd("))
        .expect("exec_cmd dispatched");
    assert!(exec.contains(&format!("--class drove-{id}")), "{exec}");
    assert!(exec.contains(&format!("DROVE_AGENT_ID={id}")), "{exec}");
    assert!(exec.contains("--settings"), "{exec}");
    assert!(exec.contains("--model x"), "{exec}");
    assert!(
        exec.contains(r#"{ workspace = "special:agents" }"#),
        "{exec}"
    );
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(env.path("data/claude-settings.json")).unwrap(),
    )
    .unwrap();
    assert!(
        settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("drove hook claude")
    );
    assert_eq!(env.agent(&id)["status"], "starting");

    // window opens
    event(
        &mut s2,
        &format!("openwindow>>abc123,special:agents,drove-{id},api"),
    );
    wait_for(
        || env.agent(&id)["window"]["address"] == "0xabc123",
        "window bound",
    );

    // hooks drive the status
    env.hook(
        "claude",
        Some(&id),
        r#"{"hook_event_name":"SessionStart","session_id":"sess-1","cwd":"/w"}"#,
    );
    let a = env.agent(&id);
    assert_eq!(a["status"], "idle");
    assert_eq!(a["session_id"], "sess-1");
    env.hook(
        "claude",
        Some(&id),
        r#"{"hook_event_name":"UserPromptSubmit","prompt":"fix it"}"#,
    );
    assert_eq!(env.agent(&id)["status"], "working");
    env.hook(
        "claude",
        Some(&id),
        r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#,
    );
    let a = env.agent(&id);
    assert_eq!(a["status"], "needs_input");
    assert_eq!(a["attention"], true);
    let table = env.ok(&["ls"]);
    assert!(table.contains("needs input !"), "{table}");
    let bar = drove::ui::bar(
        &serde_json::from_value::<Vec<drove::model::Agent>>(Value::Array(env.agents())).unwrap(),
    );
    assert_eq!(bar["class"][0], "needs-input");

    // next focuses it and clears attention
    env.ok(&["next"]);
    assert!(
        env.log("hyprctl")
            .contains(r#"hl.dsp.focus({ window = "address:0xabc123" })"#)
    );
    assert_eq!(env.agent(&id)["attention"], false);

    // turn done while another window is focused → attention; focusing clears it
    env.hook(
        "claude",
        Some(&id),
        r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
    );
    event(&mut s2, "activewindowv2>>ffff");
    wait_for(
        || {
            env.hook(
                "claude",
                Some(&id),
                r#"{"hook_event_name":"Stop","last_assistant_message":"all done"}"#,
            );
            env.agent(&id)["attention"] == true
        },
        "attention after Stop",
    );
    let a = env.agent(&id);
    assert_eq!(a["status"], "idle");
    assert_eq!(a["detail"], "all done");
    event(&mut s2, "activewindowv2>>abc123");
    wait_for(
        || env.agent(&id)["attention"] == false,
        "focus clears attention",
    );

    // terminal control
    env.ok(&["send", "api", "--enter", "hello", "world"]);
    assert_eq!(
        std::fs::read_to_string(env.path("sent.txt")).unwrap(),
        "hello world\r"
    );
    assert!(env.log("kitten").contains(&format!(
        "unix:{}",
        env.path(&format!("rt/drove/kitty-{id}.sock")).display()
    )));
    assert_eq!(env.ok(&["text", &id]), "screen text");

    // title / workspace tracking
    event(&mut s2, "windowtitlev2>>abc123,claude: busy");
    event(&mut s2, "movewindowv2>>abc123,4,4");
    wait_for(|| env.agent(&id)["window"]["workspace"] == "4", "move");
    assert_eq!(env.agent(&id)["window"]["title"], "claude: busy");

    // codex: -c notify injection + notify hook
    let cid = env
        .ok(&["spawn", "codex", "--cwd", work.to_str().unwrap()])
        .trim()
        .to_string();
    assert!(env.log("hyprctl").contains("notify=["));
    event(&mut s2, &format!("openwindow>>c0dec0,1,drove-{cid},codex"));
    wait_for(|| !env.agent(&cid)["window"].is_null(), "codex window");
    env.hook(
        "codex",
        Some(&cid),
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"th","prompt":"go"}"#,
    );
    assert_eq!(env.agent(&cid)["status"], "working");
    let o = env
        .cmd(&[
            "hook",
            "codex-notify",
            r#"{"type":"agent-turn-complete","thread-id":"th","last-assistant-message":"ok!"}"#,
        ])
        .env("DROVE_AGENT_ID", &cid)
        .output()
        .unwrap();
    assert!(o.status.success() && o.stdout.is_empty());
    assert_eq!(env.agent(&cid)["status"], "idle");
    assert_eq!(env.agent(&cid)["detail"], "ok!");
    assert_eq!(env.agent(&cid)["name"], "codex-work");

    // kiro stale tool heuristic
    let kid = env
        .ok(&["spawn", "kiro", "--cwd", work.to_str().unwrap()])
        .trim()
        .to_string();
    env.hook(
        "kiro",
        Some(&kid),
        r#"{"hook_event_name":"preToolUse","cwd":"/w","tool_name":"execute_bash"}"#,
    );
    assert_eq!(env.agent(&kid)["status"], "working");
    wait_for(|| env.agent(&kid)["status"] == "needs_input", "kiro stale");
    assert_eq!(env.agent(&kid)["maybe"], true);
    env.hook(
        "kiro",
        Some(&kid),
        r#"{"hook_event_name":"postToolUse","tool_name":"execute_bash"}"#,
    );
    assert_eq!(env.agent(&kid)["status"], "working");

    // adoption: a hook without DROVE_AGENT_ID whose ancestor (this test process) owns a window
    std::fs::write(
        env.path("clients.json"),
        format!(
            r#"[{{"address":"0xfeed","class":"kitty","initialClass":"kitty","title":"codex","pid":{},"workspace":{{"id":2,"name":"2"}},"focusHistoryID":3}}]"#,
            std::process::id()
        ),
    )
    .unwrap();
    env.hook(
        "codex",
        None,
        r#"{"hook_event_name":"SessionStart","session_id":"manual","cwd":"/src/manual"}"#,
    );
    let adopted = env
        .agents()
        .into_iter()
        .find(|a| a["adopted"] == true && a["window"]["address"] == "0xfeed")
        .expect("adopted agent");
    assert_eq!(adopted["kind"], "codex");
    assert_eq!(adopted["name"], "codex-manual");
    env.hook(
        "codex",
        None,
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"manual","prompt":"x"}"#,
    );
    assert_eq!(
        env.agent(adopted["id"].as_str().unwrap())["status"],
        "working"
    );
    // unrelated hook with no window match is dropped silently
    std::fs::write(env.path("clients.json"), "[]").unwrap();

    // close → hyprctl close; closewindow → Exited
    env.ok(&["close", "api"]);
    assert!(
        env.log("hyprctl")
            .contains(r#"hl.dsp.window.close({ window = "address:0xabc123" })"#)
    );
    event(&mut s2, "closewindow>>abc123");
    wait_for(|| env.agent(&id)["status"] == "exited", "exited");
    assert!(env.agent(&id)["window"].is_null());
    assert!(!env.ok(&["ls"]).contains(" api "));
    assert!(env.ok(&["ls", "--all"]).contains("api"));

    // restart: state persists, reconcile marks window-less agents exited
    env.stop_daemon();
    let _s2 = env.start_daemon();
    let agents = env.agents();
    assert_eq!(agents.len(), 4);
    assert!(agents.iter().all(|a| a["status"] == "exited"), "{agents:?}");
    env.ok(&["forget", "--exited"]);
    assert!(env.agents().is_empty());
}

#[test]
fn hook_is_silent_without_daemon() {
    let env = Env::new("");
    let start = Instant::now();
    env.hook("claude", Some("abcdef"), r#"{"hook_event_name":"Stop"}"#);
    env.hook("kiro", None, "not json at all");
    let o = env.run(&["hook", "bogus"]);
    assert!(o.status.success() && o.stdout.is_empty());
    let o = env.run(&["hook", "codex-notify"]);
    assert!(o.status.success() && o.stdout.is_empty());
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(!env.run(&["ls"]).status.success());
}

#[test]
fn legacy_dispatch_and_hooks_install() {
    let mut env = Env::new("");
    // Force legacy by making the no_op probe fail.
    let cfg = std::fs::read_to_string(env.path("config.toml")).unwrap();
    std::fs::write(
        env.path("config.toml"),
        cfg.replace("[hyprland]", "[hyprland]\ndispatch = \"legacy\""),
    )
    .unwrap();
    let _s2 = env.start_daemon();
    assert!(env.ok(&["status"]).contains("dispatch: legacy"));
    let work = env.path("work");
    env.ok(&[
        "spawn",
        "shell",
        "--cwd",
        work.to_str().unwrap(),
        "--workspace",
        "3 silent",
    ]);
    let hl = env.log("hyprctl");
    assert!(hl.contains("dispatch\nexec\n[workspace 3 silent] "), "{hl}");
    assert!(!hl.contains("no_op"));

    // hooks install codex into $HOME/.codex/hooks.json, idempotently
    env.ok(&["hooks", "install", "codex"]);
    let p = env.path(".codex/hooks.json");
    let first = std::fs::read_to_string(&p).unwrap();
    env.ok(&["hooks", "install", "codex"]);
    assert_eq!(std::fs::read_to_string(&p).unwrap(), first);
    let v: Value = serde_json::from_str(&first).unwrap();
    assert!(v["hooks"]["PermissionRequest"].is_array());
    std::fs::create_dir_all(env.path(".kiro/agents")).unwrap();
    std::fs::write(env.path(".kiro/agents/dev.json"), r#"{"name":"dev"}"#).unwrap();
    env.ok(&["hooks", "install", "kiro"]);
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(env.path(".kiro/agents/dev.json")).unwrap())
            .unwrap();
    assert_eq!(v["name"], "dev");
    assert!(
        v["hooks"]["preToolUse"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("hook kiro")
    );
    assert!(env.path(".kiro/agents/dev.json.drove-bak").exists());
    let printed: Value = serde_json::from_str(&env.ok(&["hooks", "print", "claude"])).unwrap();
    assert!(printed["hooks"]["Notification"].is_array());
}

#[test]
fn pick_uses_picker_command() {
    let mut env = Env::new("");
    // picker = a script that picks the first line
    script(&env.path("picker"), "head -n 1\n");
    let cfg = std::fs::read_to_string(env.path("config.toml")).unwrap();
    std::fs::write(
        env.path("config.toml"),
        format!(
            "{cfg}\n[picker]\ncommand = [\"{}\"]\n",
            env.path("picker").display()
        ),
    )
    .unwrap();
    let mut s2 = env.start_daemon();
    let work = env.path("work");
    let id = env
        .ok(&["spawn", "shell", "--cwd", work.to_str().unwrap()])
        .trim()
        .to_string();
    event(&mut s2, &format!("openwindow>>beef,1,drove-{id},sh"));
    wait_for(|| !env.agent(&id)["window"].is_null(), "window");
    assert_eq!(
        env.agent(&id)["status"],
        "idle",
        "generic agents are idle once open"
    );
    env.ok(&["pick"]);
    assert!(env.log("hyprctl").contains(r#"address:0xbeef"#));
    // bar prints a line immediately
    let mut bar = env.cmd(&["bar"]).stdout(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(bar.stdout.take().unwrap()),
        &mut line,
    )
    .unwrap();
    let _ = bar.kill();
    let _ = bar.wait();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["class"][0], "idle");
}
