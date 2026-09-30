//! kitty remote control via the `kitty` / `kitten` binaries.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::SpawnSpec;
use crate::config::TerminalConfig;
use crate::model::TermRef;
use crate::wm::agent_class;

/// `kitty …` argv for a standalone per-agent instance listening on `listen`
/// (e.g. `unix:/run/user/1000/drove/kitty-abcdef.sock`).
pub fn standalone_argv(cfg: &TerminalConfig, spec: &SpawnSpec, listen: &str) -> Vec<String> {
    let mut v: Vec<String> = vec![
        cfg.kitty.clone(),
        "--class".into(),
        agent_class(&spec.id),
        "--title".into(),
        spec.name.clone(),
        "--directory".into(),
        spec.cwd.clone(),
        "--listen-on".into(),
        listen.into(),
        "-o".into(),
        "allow_remote_control=socket-only".into(),
    ];
    v.extend(cfg.extra_args.iter().cloned());
    v.push("env".into());
    v.extend(spec.env.iter().map(|(k, val)| format!("{k}={val}")));
    v.extend(spec.argv.iter().cloned());
    v
}

/// `kitten @ launch …` argv opening an OS window in a shared kitty at `to`.
pub fn instance_argv(cfg: &TerminalConfig, spec: &SpawnSpec, to: &str) -> Vec<String> {
    let mut v: Vec<String> = vec![
        cfg.kitten.clone(),
        "@".into(),
        "--to".into(),
        to.into(),
        "launch".into(),
        "--type=os-window".into(),
        "--os-window-class".into(),
        agent_class(&spec.id),
        "--os-window-title".into(),
        spec.name.clone(),
        "--cwd".into(),
        spec.cwd.clone(),
        "--var".into(),
        format!("drove_id={}", spec.id),
    ];
    for (k, val) in &spec.env {
        v.push("--env".into());
        v.push(format!("{k}={val}"));
    }
    v.push("--".into());
    v.extend(spec.argv.iter().cloned());
    v
}

/// `kitten @ launch` prints the new window id.
pub fn parse_launch_output(out: &str) -> Option<u64> {
    out.trim().lines().last()?.trim().parse().ok()
}

fn remote(cfg: &TerminalConfig, term: &TermRef, cmd: &str) -> Vec<String> {
    let TermRef::Kitty { socket, window_id } = term;
    let mut v = vec![
        cfg.kitten.clone(),
        "@".into(),
        "--to".into(),
        socket.clone(),
        cmd.into(),
    ];
    if let Some(w) = window_id {
        v.push("--match".into());
        v.push(format!("id:{w}"));
    }
    v
}

fn run(argv: &[String], stdin: Option<&[u8]>) -> Result<String> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running {}", argv[0]))?;
    if let Some(data) = stdin {
        let mut si = child.stdin.take().unwrap();
        si.write_all(data)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "{} failed: {}",
            argv.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn send_text_argv(cfg: &TerminalConfig, term: &TermRef) -> Vec<String> {
    let mut v = remote(cfg, term, "send-text");
    // Text goes through stdin so kitty does not interpret escapes in it.
    v.push("--stdin".into());
    v
}

pub fn get_text_argv(cfg: &TerminalConfig, term: &TermRef, extent: &str) -> Vec<String> {
    let mut v = remote(cfg, term, "get-text");
    v.push("--extent".into());
    v.push(extent.into());
    v
}

pub fn send_text(cfg: &TerminalConfig, term: &TermRef, text: &str) -> Result<()> {
    run(&send_text_argv(cfg, term), Some(text.as_bytes())).map(|_| ())
}

pub fn get_text(cfg: &TerminalConfig, term: &TermRef, extent: &str) -> Result<String> {
    run(&get_text_argv(cfg, term, extent), None)
}

/// Run `kitten @ launch` (instance mode) and return the new kitty window id.
pub fn launch_instance(cfg: &TerminalConfig, spec: &SpawnSpec, to: &str) -> Result<Option<u64>> {
    let out = run(&instance_argv(cfg, spec, to), None)?;
    Ok(parse_launch_output(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SpawnSpec {
        SpawnSpec {
            id: "abcdef".into(),
            name: "api".into(),
            cwd: "/w".into(),
            env: vec![("DROVE_AGENT_ID".into(), "abcdef".into())],
            argv: vec!["claude".into(), "--settings".into(), "/s.json".into()],
        }
    }

    #[test]
    fn standalone() {
        let v = standalone_argv(
            &TerminalConfig::default(),
            &spec(),
            "unix:/r/kitty-abcdef.sock",
        );
        assert_eq!(
            v.join(" "),
            "kitty --class drove-abcdef --title api --directory /w --listen-on unix:/r/kitty-abcdef.sock \
             -o allow_remote_control=socket-only env DROVE_AGENT_ID=abcdef claude --settings /s.json"
        );
    }

    #[test]
    fn instance() {
        let v = instance_argv(&TerminalConfig::default(), &spec(), "unix:@k");
        assert_eq!(
            v.join(" "),
            "kitten @ --to unix:@k launch --type=os-window --os-window-class drove-abcdef \
             --os-window-title api --cwd /w --var drove_id=abcdef --env DROVE_AGENT_ID=abcdef \
             -- claude --settings /s.json"
        );
        assert_eq!(parse_launch_output("42\n"), Some(42));
        assert_eq!(parse_launch_output("junk"), None);
    }

    #[test]
    fn remote_cmds() {
        let cfg = TerminalConfig::default();
        let t = TermRef::Kitty {
            socket: "unix:/s".into(),
            window_id: None,
        };
        assert_eq!(
            send_text_argv(&cfg, &t).join(" "),
            "kitten @ --to unix:/s send-text --stdin"
        );
        let t = TermRef::Kitty {
            socket: "unix:@k".into(),
            window_id: Some(7),
        };
        assert_eq!(
            get_text_argv(&cfg, &t, "screen").join(" "),
            "kitten @ --to unix:@k get-text --match id:7 --extent screen"
        );
    }
}
