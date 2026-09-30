//! Blocking (no tokio) client for the daemon socket, plus the `drove hook` path.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::adapters::Source;
use crate::protocol::{Event, HookParams, Response};

pub struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Client {
    pub fn connect(path: &Path, timeout: Option<Duration>) -> Result<Self> {
        let stream = UnixStream::connect(path).with_context(|| {
            format!(
                "cannot reach the drove daemon at {} (is `drove daemon` running?)",
                path.display()
            )
        })?;
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        Ok(Client {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            next_id: 1,
        })
    }

    pub fn connect_default() -> Result<Self> {
        Self::connect(&crate::paths::socket_path(), None)
    }

    fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line =
            serde_json::to_string(&json!({"id": id, "method": method, "params": params}))?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        Ok(id)
    }

    fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("daemon closed the connection");
        }
        Ok(line)
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send(method, params)?;
        loop {
            let resp: Response = serde_json::from_str(&self.read_line()?)?;
            if resp.id != json!(id) {
                continue;
            }
            return if resp.ok {
                Ok(resp.result.unwrap_or(Value::Null))
            } else {
                Err(anyhow!(resp.error.unwrap_or_else(|| "error".into())))
            };
        }
    }

    /// Turn the connection into an event stream.
    pub fn subscribe(mut self) -> Result<impl Iterator<Item = Result<Event>>> {
        self.call("subscribe", json!({}))?;
        Ok(std::iter::from_fn(move || {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => None,
                Ok(_) => Some(serde_json::from_str(&line).map_err(Into::into)),
                Err(e) => Some(Err(e.into())),
            }
        }))
    }

    /// Like [`Client::subscribe`] but yields raw lines (for `drove watch`).
    pub fn subscribe_raw(mut self) -> Result<impl Iterator<Item = String>> {
        self.call("subscribe", json!({}))?;
        Ok(std::iter::from_fn(move || {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(line.trim_end().to_string()),
            }
        }))
    }
}

/// One-shot call against the default socket.
pub fn call(method: &str, params: Value) -> Result<Value> {
    Client::connect_default()?.call(method, params)
}

/// Parent PID from `/proc/<pid>/stat` contents.
pub fn parse_ppid(stat: &str) -> Option<i64> {
    // comm may contain spaces and parens; the state field follows the last ')'.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// PIDs of our ancestors, nearest first.
pub fn ancestor_pids() -> Vec<i64> {
    let mut out = vec![];
    let mut pid = std::os::unix::process::parent_id() as i64;
    while pid > 1 && out.len() < 64 {
        out.push(pid);
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            break;
        };
        match parse_ppid(&stat) {
            Some(p) if p != pid => pid = p,
            _ => break,
        }
    }
    out
}

const HOOK_TIMEOUT: Duration = Duration::from_millis(200);

/// `drove hook <source> [json-arg]`. Never prints to stdout, always returns 0.
pub fn hook_main(source: &str, arg: Option<&str>) -> i32 {
    let debug = std::env::var_os("DROVE_DEBUG").is_some();
    let r = std::panic::catch_unwind(|| run_hook(source, arg));
    if debug {
        match r {
            Ok(Err(e)) => eprintln!("drove hook: {e:#}"),
            Err(_) => eprintln!("drove hook: panic"),
            Ok(Ok(())) => {}
        }
    }
    0
}

fn run_hook(source: &str, arg: Option<&str>) -> Result<()> {
    let src = Source::parse(source).ok_or_else(|| anyhow!("unknown hook source {source:?}"))?;
    let payload: Value = match (src, arg) {
        (_, Some(a)) => serde_json::from_str(a)?,
        (Source::CodexNotify, None) => bail!("codex-notify needs a JSON argument"),
        _ => {
            let mut buf = String::new();
            std::io::stdin().take(4 << 20).read_to_string(&mut buf)?;
            serde_json::from_str(&buf).unwrap_or(Value::Null)
        }
    };
    let agent_id = std::env::var("DROVE_AGENT_ID")
        .ok()
        .filter(|s| !s.is_empty());
    let pids = if agent_id.is_none() {
        ancestor_pids()
    } else {
        vec![]
    };
    let params = HookParams {
        agent_id,
        source: src.as_str().to_string(),
        payload,
        pids,
    };
    let mut c = Client::connect(&crate::paths::socket_path(), Some(HOOK_TIMEOUT))?;
    c.call("hook", serde_json::to_value(params)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppid() {
        assert_eq!(parse_ppid("123 (bash) S 45 123 123 0 -1"), Some(45));
        assert_eq!(parse_ppid("9 (we ird) (x) R 7 1 1"), Some(7));
        assert_eq!(parse_ppid("garbage"), None);
    }

    #[test]
    fn ancestors_include_parent() {
        let a = ancestor_pids();
        assert_eq!(
            a.first().copied(),
            Some(std::os::unix::process::parent_id() as i64)
        );
    }
}
