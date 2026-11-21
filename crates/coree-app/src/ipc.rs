// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust, zero-`unsafe` single-instance desktop IPC handoff over loopback TCP +
//! atomic lockfile (`<data_local_dir>/coree/instance.lock`).
//!
//! When a user double-clicks a `.cdr` or `.coree` file in Explorer / Nautilus / Dolphin
//! while an instance of `coree` is already open, the secondary launch forwards the
//! file paths + authentication token to the running studio and exits cleanly, while
//! the active window focuses itself and opens the files as new document tabs.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_IPC_LINE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstanceLockFile {
    port: u16,
    pid: u32,
    token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IpcHandoffRequest {
    token: String,
    files: Vec<String>,
}

/// Returns the default per-user lockfile path used for single-instance handoff.
#[must_use]
pub fn default_lock_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("coree")
        .join("instance.lock")
}

/// Attempts to forward `files` (or a window-focus signal when `files` is empty) to an
/// already-running `coree` instance at [`default_lock_path`].
#[must_use]
pub fn try_handoff_to_existing(files: &[PathBuf]) -> bool {
    try_handoff_at(&default_lock_path(), files)
}

/// Attempts to forward `files` to a running instance registered at `lock_path`.
/// Returns `true` only after receiving an authenticated `"OK"` acknowledgement.
#[must_use]
pub fn try_handoff_at(lock_path: &Path, files: &[PathBuf]) -> bool {
    let Ok(raw) = fs::read_to_string(lock_path) else {
        return false;
    };
    let Ok(lock) = serde_json::from_str::<InstanceLockFile>(&raw) else {
        let _ = fs::remove_file(lock_path);
        return false;
    };

    let addr = SocketAddr::from(([127, 0, 0, 1], lock.port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) else {
        remove_lock_if_token_matches(lock_path, &lock.token);
        return false;
    };
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let req = IpcHandoffRequest {
        token: lock.token.clone(),
        files: absolutize_paths(files),
    };
    let Ok(mut line) = serde_json::to_string(&req) else {
        return false;
    };
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() || stream.flush().is_err() {
        remove_lock_if_token_matches(lock_path, &lock.token);
        return false;
    }

    let mut resp = String::new();
    let mut reader = BufReader::new(stream.take(128));
    if reader.read_line(&mut resp).is_err() {
        remove_lock_if_token_matches(lock_path, &lock.token);
        return false;
    }
    if resp.trim() == "OK" {
        true
    } else {
        remove_lock_if_token_matches(lock_path, &lock.token);
        false
    }
}

/// Running single-instance loopback listener bound to an active studio window.
pub struct IpcServer {
    lock_path: PathBuf,
    port: u16,
    token: String,
    rx: Receiver<Vec<PathBuf>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl IpcServer {
    /// Binds a single-instance IPC listener at [`default_lock_path`], waking `wake_ctx`
    /// whenever an incoming launch request is received.
    pub fn bind_default_with_wake(wake_ctx: Option<egui::Context>) -> Result<Self, String> {
        Self::bind_at_with_wake(&default_lock_path(), wake_ctx)
    }

    /// Binds a single-instance IPC listener at `lock_path` without an `egui` wake hook.
    pub fn bind_at(lock_path: &Path) -> Result<Self, String> {
        Self::bind_at_with_wake(lock_path, None)
    }

    /// Binds a single-instance IPC listener at `lock_path` with an optional `egui` repaint hook.
    pub fn bind_at_with_wake(
        lock_path: &Path,
        wake_ctx: Option<egui::Context>,
    ) -> Result<Self, String> {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create IPC dir {}: {e}", parent.display()))?;
        }

        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .map_err(|e| format!("failed to bind IPC loopback socket: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("failed to read IPC local address: {e}"))?
            .port();
        let token = Uuid::new_v4().to_string();

        let lock_payload = InstanceLockFile {
            port,
            pid: std::process::id(),
            token: token.clone(),
        };
        let encoded = serde_json::to_string(&lock_payload)
            .map_err(|e| format!("failed to serialize IPC lockfile: {e}"))?;
        write_lockfile_atomic(lock_path, &encoded)?;

        let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let expected_token = token.clone();

        let handle = thread::Builder::new()
            .name("coree-ipc-listener".to_owned())
            .spawn(move || {
                while !stop_flag.load(Ordering::SeqCst) {
                    let Ok((mut stream, _peer)) = listener.accept() else {
                        break;
                    };
                    if stop_flag.load(Ordering::SeqCst) {
                        let _ = stream.shutdown(Shutdown::Both);
                        break;
                    }
                    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
                    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

                    let mut line = String::new();
                    let read_ok = {
                        let mut bounded = BufReader::new(&stream).take(MAX_IPC_LINE_BYTES);
                        bounded.read_line(&mut line).is_ok()
                    };
                    if !read_ok {
                        continue;
                    }
                    let Ok(req) = serde_json::from_str::<IpcHandoffRequest>(line.trim()) else {
                        continue;
                    };
                    if req.token != expected_token {
                        continue;
                    }
                    let paths: Vec<PathBuf> = req
                        .files
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .collect();
                    let _ = tx.send(paths);
                    let _ = stream.write_all(b"OK\n");
                    let _ = stream.flush();
                    let _ = stream.shutdown(Shutdown::Both);
                    if let Some(ref ctx) = wake_ctx {
                        ctx.request_repaint();
                    }
                }
            })
            .map_err(|e| format!("failed to spawn IPC listener thread: {e}"))?;

        Ok(Self {
            lock_path: lock_path.to_path_buf(),
            port,
            token,
            rx,
            stop,
            handle: Some(handle),
        })
    }

    /// Returns the ephemeral localhost port bound by this instance.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Non-blockingly drains all pending file-open / focus batches received over IPC.
    #[must_use]
    pub fn try_recv_batches(&self) -> Vec<Vec<PathBuf>> {
        let mut out = Vec::new();
        while let Ok(batch) = self.rx.try_recv() {
            out.push(batch);
        }
        out
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(100));
        remove_lock_if_token_matches(&self.lock_path, &self.token);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn absolutize_paths(files: &[PathBuf]) -> Vec<String> {
    let cwd = std::env::current_dir().ok();
    files
        .iter()
        .map(|p| {
            let resolved = if p.is_relative() {
                cwd.as_ref().map_or_else(|| p.clone(), |base| base.join(p))
            } else {
                p.clone()
            };
            resolved.to_string_lossy().into_owned()
        })
        .collect()
}

fn remove_lock_if_token_matches(lock_path: &Path, expected_token: &str) {
    if let Ok(raw) = fs::read_to_string(lock_path) {
        if let Ok(lock) = serde_json::from_str::<InstanceLockFile>(&raw) {
            if lock.token == expected_token {
                let _ = fs::remove_file(lock_path);
            }
        }
    }
}

fn write_lockfile_atomic(lock_path: &Path, contents: &str) -> Result<(), String> {
    let tmp = lock_path.with_extension(format!("lock.tmp.{}", std::process::id()));
    fs::write(&tmp, contents.as_bytes())
        .map_err(|e| format!("failed to write temp lockfile {}: {e}", tmp.display()))?;
    if fs::rename(&tmp, lock_path).is_err() {
        fs::write(lock_path, contents.as_bytes()).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("failed to write lockfile {}: {e}", lock_path.display())
        })?;
        let _ = fs::remove_file(&tmp);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_single_instance_handoff_and_stale_cleanup_work() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("instance.lock");

        // No lockfile yet -> handoff returns false.
        assert!(!try_handoff_at(
            &lock_path,
            &[PathBuf::from("/tmp/تصميم/card.cdr")]
        ));

        {
            let server = IpcServer::bind_at(&lock_path).expect("bind IPC server");
            assert!(server.port() > 0);
            assert!(lock_path.exists());

            // Handoff with Arabic UTF-8 filename delivers paths to server and acknowledges.
            let handed = try_handoff_at(
                &lock_path,
                &[
                    PathBuf::from("/tmp/شعار/poster.cdr"),
                    PathBuf::from("/tmp/badge.coree"),
                ],
            );
            assert!(handed, "live IPC server must acknowledge handoff");

            let batches = server.try_recv_batches();
            assert_eq!(batches.len(), 1);
            assert_eq!(
                batches[0],
                vec![
                    PathBuf::from("/tmp/شعار/poster.cdr"),
                    PathBuf::from("/tmp/badge.coree"),
                ]
            );

            // Empty-file focus ping also hands off cleanly.
            assert!(try_handoff_at(&lock_path, &[]));
            let focus_batches = server.try_recv_batches();
            assert_eq!(focus_batches, vec![Vec::<PathBuf>::new()]);
        }

        // Dropping the server removes the lockfile cleanly.
        assert!(!lock_path.exists());

        // Stale lockfile left by a crashed process must return false and self-clean.
        let stale = serde_json::json!({
            "port": 1,
            "pid": 999_999u32,
            "token": "stale-token"
        });
        fs::write(&lock_path, stale.to_string()).expect("write stale lock");
        assert!(!try_handoff_at(&lock_path, &[]));
        assert!(!lock_path.exists(), "stale lockfile must be cleaned up");
    }
}
