#![allow(dead_code)]
//! Shared helpers: run tools/mock-drove.py and inspect its request log.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Mock {
    child: Child,
    pub socket: PathBuf,
    pub log: PathBuf,
    pub dir: PathBuf,
}

impl Mock {
    pub fn start(tag: &str, tick: f64) -> Mock {
        let dir = std::env::temp_dir().join(format!("drove-notify-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let log = dir.join("requests.log");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/mock-drove.py");
        let child = Command::new("python3")
            .arg(script)
            .args(["--socket"])
            .arg(&socket)
            .args(["--tick", &tick.to_string(), "--log"])
            .arg(&log)
            .stdout(Stdio::null())
            .spawn()
            .expect("python3 available");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "mock did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        Mock {
            child,
            socket,
            log,
            dir,
        }
    }

    /// Parsed request log lines.
    pub fn requests(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    pub fn focus_requests(&self) -> Vec<String> {
        self.requests()
            .iter()
            .filter(|r| r["method"] == "focus")
            .filter_map(|r| r["params"]["id"].as_str().map(str::to_string))
            .collect()
    }

    pub fn wait_focus(&self, id: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.focus_requests().iter().any(|f| f == id) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
