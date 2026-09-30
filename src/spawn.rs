//! Building what a spawned agent runs: argv with hook injections, worktrees, names.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::Profile;
use crate::model::AgentKind;

/// TOML value for codex `-c notify=…` (JSON strings are valid TOML basic strings).
pub fn codex_notify_override(drove: &str) -> String {
    let arr = serde_json::to_string(&[drove, "hook", "codex-notify"]).unwrap();
    format!("notify={arr}")
}

/// The agent's argv: profile command + drove's injections + user args.
/// `claude_settings` is the generated `--settings` file (Claude only).
pub fn agent_argv(
    profile: &Profile,
    extra: &[String],
    drove: &str,
    claude_settings: Option<&Path>,
) -> Result<Vec<String>> {
    let Some((prog, rest)) = profile.command.split_first() else {
        bail!("profile has an empty command");
    };
    let mut v = vec![prog.clone()];
    match profile.kind {
        AgentKind::Claude => {
            if let Some(p) = claude_settings {
                v.push("--settings".into());
                v.push(p.to_string_lossy().into_owned());
            }
        }
        AgentKind::Codex if profile.inject => {
            v.push("-c".into());
            v.push(codex_notify_override(drove));
        }
        _ => {}
    }
    v.extend(rest.iter().cloned());
    v.extend(extra.iter().cloned());
    Ok(v)
}

/// Branch names may contain `/`; keep worktree directories flat.
pub fn sanitize_branch(b: &str) -> String {
    b.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn worktree_path(template: &str, repo: &Path, branch: &str) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let s = template
        .replace("{repo}", &repo.to_string_lossy())
        .replace("{repo_name}", &repo_name)
        .replace("{branch}", &sanitize_branch(branch));
    normalize(Path::new(&s))
}

/// Lexically resolve `.` and `..` (paths may not exist yet).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

fn git(dir: &Path, args: &[&str]) -> Result<(bool, String)> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("running git")?;
    let text = if out.status.success() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        String::from_utf8_lossy(&out.stderr).trim().to_string()
    };
    Ok((out.status.success(), text))
}

/// Create (or reuse) a worktree for `branch` of the repo containing `cwd`.
pub fn ensure_worktree(cwd: &Path, branch: &str, template: &str) -> Result<PathBuf> {
    let (ok, top) = git(cwd, &["rev-parse", "--show-toplevel"])?;
    if !ok {
        bail!("{} is not inside a git repository: {top}", cwd.display());
    }
    let repo = PathBuf::from(top);
    let path = worktree_path(template, &repo, branch);
    if path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let p = path.to_string_lossy();
    let (exists, _) = git(
        &repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let (ok, err) = if exists {
        git(&repo, &["worktree", "add", &p, branch])?
    } else {
        git(&repo, &["worktree", "add", "-b", branch, &p])?
    };
    if !ok {
        bail!("git worktree add failed: {err}");
    }
    Ok(path)
}

pub fn default_name(profile: &str, cwd: &str) -> String {
    let base = Path::new(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    match base {
        Some(b) => format!("{profile}-{b}"),
        None => profile.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn profile(kind: AgentKind, cmd: &[&str]) -> Profile {
        Profile {
            kind,
            command: cmd.iter().map(|s| s.to_string()).collect(),
            workspace: None,
            env: BTreeMap::new(),
            inject: true,
        }
    }

    #[test]
    fn claude_argv() {
        let v = agent_argv(
            &profile(AgentKind::Claude, &["claude", "--model", "opus"]),
            &["-c".into()],
            "/bin/drove",
            Some(Path::new("/d/claude-settings.json")),
        )
        .unwrap();
        assert_eq!(
            v,
            [
                "claude",
                "--settings",
                "/d/claude-settings.json",
                "--model",
                "opus",
                "-c"
            ]
        );
    }

    #[test]
    fn codex_argv() {
        let v = agent_argv(
            &profile(AgentKind::Codex, &["codex"]),
            &[],
            "/bin/dr\"ove",
            None,
        )
        .unwrap();
        assert_eq!(
            v,
            [
                "codex",
                "-c",
                r#"notify=["/bin/dr\"ove","hook","codex-notify"]"#
            ]
        );
        let mut p = profile(AgentKind::Codex, &["codex"]);
        p.inject = false;
        assert_eq!(agent_argv(&p, &[], "/d", None).unwrap(), ["codex"]);
    }

    #[test]
    fn kiro_and_generic_argv() {
        let v = agent_argv(
            &profile(AgentKind::Kiro, &["kiro-cli", "chat"]),
            &[],
            "/d",
            None,
        )
        .unwrap();
        assert_eq!(v, ["kiro-cli", "chat"]);
        assert!(agent_argv(&profile(AgentKind::Generic, &[]), &[], "/d", None).is_err());
    }

    #[test]
    fn worktree_paths() {
        assert_eq!(
            worktree_path(
                "{repo}/../{repo_name}.worktrees/{branch}",
                Path::new("/src/app"),
                "feat/x y"
            ),
            PathBuf::from("/src/app.worktrees/feat-x-y")
        );
    }

    #[test]
    fn names() {
        assert_eq!(default_name("claude", "/src/app"), "claude-app");
        assert_eq!(default_name("claude", "/"), "claude");
    }

    #[test]
    fn real_worktree() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("app");
        std::fs::create_dir(&repo).unwrap();
        let g = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .env("GIT_AUTHOR_NAME", "t")
                    .env("GIT_AUTHOR_EMAIL", "t@t")
                    .env("GIT_COMMITTER_NAME", "t")
                    .env("GIT_COMMITTER_EMAIL", "t@t")
                    .output()
                    .unwrap()
                    .status
                    .success()
            )
        };
        g(&["init", "-q"]);
        g(&["commit", "-q", "--allow-empty", "-m", "init"]);
        let tpl = "{repo}/../{repo_name}.worktrees/{branch}";
        let p = ensure_worktree(&repo, "feat/one", tpl).unwrap();
        assert!(p.join(".git").exists());
        assert!(p.ends_with("app.worktrees/feat-one"));
        // reuse
        assert_eq!(ensure_worktree(&repo, "feat/one", tpl).unwrap(), p);
        assert!(ensure_worktree(dir.path(), "x", tpl).is_err());
    }
}
