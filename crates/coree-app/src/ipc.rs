// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust, zero-`unsafe`, zero-`std::net` single-instance desktop & automation IPC
//! over owner-only Unix Domain Sockets (`0o700` dir / `0o600` socket) on Unix and
//! atomic per-instance spool-directory maildrops (`<lock>.spool`) on Windows.
//!
//! Supports both single-instance file-open handoffs (`app.open_files`) and
//! bidirectional JSON-RPC `2.0` Scene-DOM / `CommandHistory` commands (`studio.ping`,
//! `scene.get_document`, `scene.list_objects`, `scene.get_object`,
//! `scene.execute_command`, `scene.execute_batch`, `scene.undo`, `scene.redo`,
//! `scene.history`).

use coree_core::{CommandHistory, CommandOp, Document, ObjectId};
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use uuid::Uuid;

const IO_TIMEOUT: Duration = Duration::from_millis(1500);
const RPC_DISPATCH_TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_IPC_LINE_BYTES: u64 = 1024 * 1024;
#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 100;

pub const JSONRPC_PARSE_ERROR: i32 = -32700;
pub const JSONRPC_INVALID_REQUEST: i32 = -32600;
pub const JSONRPC_METHOD_NOT_FOUND: i32 = -32601;
pub const JSONRPC_INVALID_PARAMS: i32 = -32602;
pub const JSONRPC_SERVER_ERROR: i32 = -32000;
pub const JSONRPC_UNAUTHORIZED: i32 = -32001;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstanceLockFile {
    #[serde(default)]
    socket_path: String,
    #[serde(default)]
    pipe_name: String,
    #[serde(default)]
    spool_dir: String,
    pid: u32,
    token: String,
}

/// JSON-RPC `2.0` request payload exchanged over UDS / Named Pipe IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default = "default_jsonrpc_version")]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

fn default_jsonrpc_version() -> String {
    "2.0".to_owned()
}

impl JsonRpcRequest {
    #[must_use]
    pub fn new(
        id: impl Into<serde_json::Value>,
        method: impl Into<String>,
        params: serde_json::Value,
    ) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id: Some(id.into()),
            method: method.into(),
            params,
            token: None,
        }
    }
}

/// JSON-RPC `2.0` error object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// JSON-RPC `2.0` response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    #[must_use]
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub fn err(id: serde_json::Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Pending bidirectional JSON-RPC call queued for main-thread `StudioApp` execution.
pub struct PendingIpcCall {
    pub request: JsonRpcRequest,
    pub reply_tx: SyncSender<JsonRpcResponse>,
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
/// Returns `true` only after receiving an authenticated JSON-RPC `app.open_files` acknowledgement.
#[must_use]
pub fn try_handoff_at(lock_path: &Path, files: &[PathBuf]) -> bool {
    let Ok(raw) = fs::read_to_string(lock_path) else {
        return false;
    };
    let Ok(lock) = serde_json::from_str::<InstanceLockFile>(&raw) else {
        let _ = fs::remove_file(lock_path);
        return false;
    };

    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: Some(serde_json::json!(1)),
        method: "app.open_files".to_owned(),
        params: serde_json::json!({ "files": absolutize_paths(files) }),
        token: Some(lock.token.clone()),
    };
    match send_jsonrpc_with_lock(&lock, &req) {
        Ok(resp) => {
            let accepted = resp
                .result
                .as_ref()
                .and_then(|r| r.get("accepted"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if accepted {
                true
            } else {
                remove_lock_if_token_matches(lock_path, &lock.token);
                false
            }
        }
        Err(_) => {
            remove_lock_if_token_matches(lock_path, &lock.token);
            false
        }
    }
}

/// Sends a bidirectional JSON-RPC `2.0` request to the running studio instance registered at `lock_path`.
pub fn send_jsonrpc_at(lock_path: &Path, req: &JsonRpcRequest) -> Result<JsonRpcResponse, String> {
    let raw = fs::read_to_string(lock_path)
        .map_err(|e| format!("failed to read IPC lockfile {}: {e}", lock_path.display()))?;
    let lock = serde_json::from_str::<InstanceLockFile>(&raw)
        .map_err(|e| format!("invalid IPC lockfile {}: {e}", lock_path.display()))?;

    let mut outgoing = req.clone();
    if outgoing.token.is_none() && outgoing.params.get("token").is_none() {
        outgoing.token = Some(lock.token.clone());
    }
    send_jsonrpc_with_lock(&lock, &outgoing)
}

/// Sends a bidirectional JSON-RPC `2.0` request to the default studio instance.
pub fn send_jsonrpc_default(req: &JsonRpcRequest) -> Result<JsonRpcResponse, String> {
    send_jsonrpc_at(&default_lock_path(), req)
}

fn send_jsonrpc_with_lock(
    lock: &InstanceLockFile,
    req: &JsonRpcRequest,
) -> Result<JsonRpcResponse, String> {
    let mut line = serde_json::to_string(req).map_err(|e| format!("serialize RPC: {e}"))?;
    line.push('\n');

    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        if lock.socket_path.is_empty() {
            return Err("lockfile missing Unix socket_path".to_owned());
        }
        let mut stream = UnixStream::connect(&lock.socket_path)
            .map_err(|e| format!("UDS connect failed ({}): {e}", lock.socket_path))?;
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        stream
            .write_all(line.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| format!("UDS write failed: {e}"))?;

        let mut resp_line = String::new();
        let mut reader = BufReader::new(stream.take(MAX_IPC_LINE_BYTES));
        reader
            .read_line(&mut resp_line)
            .map_err(|e| format!("UDS read failed: {e}"))?;
        if resp_line.trim().is_empty() {
            return Err("empty UDS JSON-RPC response".to_owned());
        }
        serde_json::from_str::<JsonRpcResponse>(resp_line.trim())
            .map_err(|e| format!("invalid JSON-RPC response: {e}"))
    }

    #[cfg(windows)]
    {
        if lock.spool_dir.is_empty() {
            return Err("lockfile missing Windows IPC spool_dir".to_owned());
        }
        spool_dir_roundtrip(Path::new(&lock.spool_dir), &line)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = lock;
        let _ = line;
        Err("IPC transport unsupported on this target".to_owned())
    }
}

/// Dispatches a Scene-DOM / `CommandHistory` JSON-RPC `2.0` request directly against `doc` and `history`.
#[must_use]
pub fn dispatch_jsonrpc_on_session(
    req: &JsonRpcRequest,
    doc: &mut Document,
    history: &mut CommandHistory,
) -> JsonRpcResponse {
    let id = req.id.clone().unwrap_or(serde_json::Value::Null);
    if req.jsonrpc != "2.0" {
        return JsonRpcResponse::err(id, JSONRPC_INVALID_REQUEST, "jsonrpc must be '2.0'");
    }
    match req.method.as_str() {
        "studio.ping" => JsonRpcResponse::ok(
            id,
            serde_json::json!({
                "ok": true,
                "version": env!("CARGO_PKG_VERSION"),
            }),
        ),
        "scene.get_document" => {
            let pages: Vec<serde_json::Value> = doc
                .pages()
                .iter()
                .enumerate()
                .map(|(idx, p)| {
                    serde_json::json!({
                        "index": idx,
                        "id": p.id.0,
                        "name": p.name,
                        "width_mm": p.width_mm,
                        "height_mm": p.height_mm,
                        "layer_count": p.layers.len(),
                    })
                })
                .collect();
            JsonRpcResponse::ok(
                id,
                serde_json::json!({
                    "id": doc.id().to_string(),
                    "title": doc.title(),
                    "color_mode": format!("{:?}", doc.color_mode()),
                    "active_page_index": doc.active_page_index(),
                    "page_count": doc.pages().len(),
                    "object_count": doc.objects().len(),
                    "next_id": doc.next_id(),
                    "pages": pages,
                }),
            )
        }
        "scene.list_objects" => {
            let objs: Vec<serde_json::Value> = doc
                .objects()
                .values()
                .map(|o| {
                    let bounds = doc.world_object_bounds(o.id).map(|b| {
                        serde_json::json!({
                            "min_x": b.min_x,
                            "min_y": b.min_y,
                            "max_x": b.max_x,
                            "max_y": b.max_y,
                        })
                    });
                    serde_json::json!({
                        "id": o.id.0,
                        "name": o.name,
                        "visible": o.visible,
                        "locked": o.locked,
                        "printable": o.printable,
                        "bounds": bounds,
                    })
                })
                .collect();
            JsonRpcResponse::ok(id, serde_json::json!({ "objects": objs }))
        }
        "scene.get_object" => {
            let Some(oid_val) = req.params.get("id").and_then(serde_json::Value::as_u64) else {
                return JsonRpcResponse::err(
                    id,
                    JSONRPC_INVALID_PARAMS,
                    "missing integer params.id",
                );
            };
            let oid = ObjectId(oid_val);
            match doc.object(oid) {
                Some(obj) => match serde_json::to_value(obj) {
                    Ok(v) => JsonRpcResponse::ok(id, v),
                    Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e.to_string()),
                },
                None => JsonRpcResponse::err(
                    id,
                    JSONRPC_SERVER_ERROR,
                    format!("object {} not found", oid.0),
                ),
            }
        }
        "scene.execute_command" => {
            let Some(op_val) = req.params.get("op") else {
                return JsonRpcResponse::err(id, JSONRPC_INVALID_PARAMS, "missing params.op");
            };
            let op: CommandOp = match serde_json::from_value(op_val.clone()) {
                Ok(op) => op,
                Err(e) => {
                    return JsonRpcResponse::err(
                        id,
                        JSONRPC_INVALID_PARAMS,
                        format!("invalid CommandOp: {e}"),
                    );
                }
            };
            match history.execute(doc, op) {
                Ok(()) => JsonRpcResponse::ok(
                    id,
                    serde_json::json!({
                        "applied": true,
                        "next_id": doc.next_id(),
                        "object_count": doc.objects().len(),
                        "undo_depth": history.undo_labels().len(),
                    }),
                ),
                Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e.to_string()),
            }
        }
        "scene.execute_batch" => {
            let Some(ops_val) = req.params.get("ops") else {
                return JsonRpcResponse::err(id, JSONRPC_INVALID_PARAMS, "missing params.ops");
            };
            let ops: Vec<CommandOp> = match serde_json::from_value(ops_val.clone()) {
                Ok(ops) => ops,
                Err(e) => {
                    return JsonRpcResponse::err(
                        id,
                        JSONRPC_INVALID_PARAMS,
                        format!("invalid CommandOp array: {e}"),
                    );
                }
            };
            match history.execute(doc, CommandOp::Batch(ops)) {
                Ok(()) => JsonRpcResponse::ok(
                    id,
                    serde_json::json!({
                        "applied": true,
                        "next_id": doc.next_id(),
                        "object_count": doc.objects().len(),
                        "undo_depth": history.undo_labels().len(),
                    }),
                ),
                Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e.to_string()),
            }
        }
        "scene.undo" => match history.undo(doc) {
            Ok(label) => JsonRpcResponse::ok(
                id,
                serde_json::json!({
                    "undone": true,
                    "label": label,
                    "object_count": doc.objects().len(),
                    "undo_depth": history.undo_labels().len(),
                    "redo_depth": history.redo_labels().len(),
                }),
            ),
            Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e.to_string()),
        },
        "scene.redo" => match history.redo(doc) {
            Ok(label) => JsonRpcResponse::ok(
                id,
                serde_json::json!({
                    "redone": true,
                    "label": label,
                    "object_count": doc.objects().len(),
                    "undo_depth": history.undo_labels().len(),
                    "redo_depth": history.redo_labels().len(),
                }),
            ),
            Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e.to_string()),
        },
        "scene.history" => JsonRpcResponse::ok(
            id,
            serde_json::json!({
                "undo": history.undo_labels(),
                "redo": history.redo_labels(),
            }),
        ),
        "scene.query_cql" => {
            let query = req
                .params
                .get("query")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            match coree_prod::evaluate_cql_query(doc, coree_prod::FindScope::WholeDocument, query) {
                Ok(ids) => JsonRpcResponse::ok(
                    id,
                    serde_json::json!({
                        "object_ids": ids.into_iter().map(|o| o.0).collect::<Vec<u64>>(),
                    }),
                ),
                Err(e) => JsonRpcResponse::err(id, JSONRPC_INVALID_PARAMS, e.to_string()),
            }
        }
        "scene.run_macro" => {
            let script = req
                .params
                .get("script")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if script.trim().is_empty() {
                return JsonRpcResponse::err(id, JSONRPC_INVALID_PARAMS, "missing params.script");
            }
            let sandbox = coree_prod::MacroSandboxConfig::default();
            match coree_prod::plan_macro_ops(
                doc,
                script,
                &std::collections::BTreeMap::new(),
                &sandbox,
            ) {
                Ok((res, ops)) => {
                    let applied = if ops.is_empty() {
                        Ok(())
                    } else {
                        history
                            .execute(doc, CommandOp::Batch(ops))
                            .map_err(|e| e.to_string())
                    };
                    match applied {
                        Ok(()) => JsonRpcResponse::ok(
                            id,
                            serde_json::json!({
                                "commands_executed": res.commands_executed,
                                "logs": res.logs,
                                "object_count": doc.objects().len(),
                                "undo_depth": history.undo_labels().len(),
                            }),
                        ),
                        Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e),
                    }
                }
                Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e.to_string()),
            }
        }
        "scene.preflight" => {
            let mut rules = coree_prod::PreflightRules::default();
            if let Some(min_dpi) = req
                .params
                .get("min_dpi")
                .and_then(serde_json::Value::as_f64)
            {
                if min_dpi.is_finite() && min_dpi > 0.0 {
                    rules.min_dpi = min_dpi;
                }
            }
            let report = coree_prod::run_preflight(doc, &rules);
            match serde_json::to_value(&report) {
                Ok(val) => JsonRpcResponse::ok(id, val),
                Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e.to_string()),
            }
        }
        other => JsonRpcResponse::err(
            id,
            JSONRPC_METHOD_NOT_FOUND,
            format!("unknown JSON-RPC method '{other}'"),
        ),
    }
}

/// Running single-instance & JSON-RPC IPC listener bound to an active studio window.
pub struct IpcServer {
    lock_path: PathBuf,
    socket_path: PathBuf,
    #[cfg_attr(not(windows), allow(dead_code))]
    spool_dir: PathBuf,
    pipe_name: String,
    token: String,
    rx_batches: Mutex<Receiver<Vec<PathBuf>>>,
    rx_rpc: Mutex<Receiver<PendingIpcCall>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl IpcServer {
    /// Binds a single-instance IPC listener at [`default_lock_path`], waking `wake_ctx`
    /// whenever an incoming launch or JSON-RPC request arrives.
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
        ensure_private_parent_dir(lock_path)?;

        let socket_path = lock_path.with_extension("sock");
        let spool_dir = lock_path.with_extension("spool");
        let token = Uuid::new_v4().to_string();
        let pipe_name = format!(r"\\.\pipe\coree-{}-{}", std::process::id(), &token[..8]);

        let (tx_batches, rx_batches) = mpsc::channel::<Vec<PathBuf>>();
        let (tx_rpc, rx_rpc) = mpsc::channel::<PendingIpcCall>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let expected_token = token.clone();

        #[cfg(unix)]
        let handle = {
            use std::os::unix::fs::PermissionsExt as _;
            use std::os::unix::net::{UnixListener, UnixStream};

            let sock_str = socket_path.to_string_lossy();
            if sock_str.len() >= MAX_UNIX_SOCKET_PATH_BYTES {
                return Err(format!(
                    "Unix socket path exceeds {} bytes ({}): {}",
                    MAX_UNIX_SOCKET_PATH_BYTES,
                    sock_str.len(),
                    socket_path.display()
                ));
            }

            if socket_path.exists() {
                if UnixStream::connect(&socket_path).is_ok() {
                    return Err(format!(
                        "IPC server already running at {}",
                        socket_path.display()
                    ));
                }
                let _ = fs::remove_file(&socket_path);
            }

            let listener = UnixListener::bind(&socket_path).map_err(|e| {
                format!(
                    "failed to bind Unix IPC socket {}: {e}",
                    socket_path.display()
                )
            })?;
            fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).map_err(|e| {
                format!(
                    "failed to set 0o600 permissions on {}: {e}",
                    socket_path.display()
                )
            })?;

            thread::Builder::new()
                .name("coree-ipc-uds".to_owned())
                .spawn(move || {
                    while !stop_flag.load(Ordering::SeqCst) {
                        let Ok((stream, _peer)) = listener.accept() else {
                            break;
                        };
                        if stop_flag.load(Ordering::SeqCst) {
                            break;
                        }
                        handle_stream_connection(
                            stream,
                            &expected_token,
                            &tx_batches,
                            &tx_rpc,
                            &wake_ctx,
                            &stop_flag,
                        );
                    }
                })
                .map_err(|e| format!("failed to spawn UDS IPC thread: {e}"))?
        };

        #[cfg(windows)]
        let handle = {
            if lock_path.exists() {
                let ping = JsonRpcRequest::new(0, "studio.ping", serde_json::json!({}));
                if send_jsonrpc_at(lock_path, &ping).is_ok() {
                    return Err(format!(
                        "IPC server already running at {}",
                        lock_path.display()
                    ));
                }
            }
            if spool_dir.exists() {
                let _ = fs::remove_dir_all(&spool_dir);
            }
            fs::create_dir_all(&spool_dir).map_err(|e| {
                format!(
                    "failed to create Windows IPC spool dir {}: {e}",
                    spool_dir.display()
                )
            })?;
            let spool_clone = spool_dir.clone();
            thread::Builder::new()
                .name("coree-ipc-spool".to_owned())
                .spawn(move || {
                    serve_spool_dir_loop(
                        &spool_clone,
                        &expected_token,
                        &tx_batches,
                        &tx_rpc,
                        &wake_ctx,
                        &stop_flag,
                    );
                })
                .map_err(|e| format!("failed to spawn Windows IPC thread: {e}"))?
        };

        #[cfg(not(any(unix, windows)))]
        let handle = thread::Builder::new()
            .name("coree-ipc-noop".to_owned())
            .spawn(move || {
                let _ = (stop_flag, expected_token, tx_batches, tx_rpc, wake_ctx);
            })
            .map_err(|e| format!("failed to spawn IPC thread: {e}"))?;

        let lock_payload = InstanceLockFile {
            socket_path: socket_path.to_string_lossy().into_owned(),
            pipe_name: pipe_name.clone(),
            spool_dir: spool_dir.to_string_lossy().into_owned(),
            pid: std::process::id(),
            token: token.clone(),
        };
        let encoded = serde_json::to_string(&lock_payload)
            .map_err(|e| format!("failed to serialize IPC lockfile: {e}"))?;
        write_lockfile_atomic(lock_path, &encoded)?;

        Ok(Self {
            lock_path: lock_path.to_path_buf(),
            socket_path,
            spool_dir,
            pipe_name,
            token,
            rx_batches: Mutex::new(rx_batches),
            rx_rpc: Mutex::new(rx_rpc),
            stop,
            handle: Some(handle),
        })
    }

    /// Returns the Unix domain socket path bound by this instance.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Returns the lockfile path bound by this instance.
    #[must_use]
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Returns the Windows Named Pipe path associated with this instance.
    #[must_use]
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// Returns the authentication token for this IPC server.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Non-blockingly drains all pending file-open / focus batches received over IPC.
    #[must_use]
    pub fn try_recv_batches(&self) -> Vec<Vec<PathBuf>> {
        let mut out = Vec::new();
        if let Ok(rx) = self.rx_batches.lock() {
            while let Ok(batch) = rx.try_recv() {
                out.push(batch);
            }
        }
        out
    }

    /// Non-blockingly drains all pending bidirectional JSON-RPC requests queued for `StudioApp`.
    #[must_use]
    pub fn try_recv_rpc(&self) -> Vec<PendingIpcCall> {
        let mut out = Vec::new();
        if let Ok(rx) = self.rx_rpc.lock() {
            while let Ok(call) = rx.try_recv() {
                out.push(call);
            }
        }
        out
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        #[cfg(unix)]
        {
            let _ = std::os::unix::net::UnixStream::connect(&self.socket_path);
            let _ = fs::remove_file(&self.socket_path);
        }
        remove_lock_if_token_matches(&self.lock_path, &self.token);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        #[cfg(windows)]
        {
            let _ = fs::remove_dir_all(&self.spool_dir);
        }
    }
}

#[cfg(unix)]
fn handle_stream_connection(
    mut stream: std::os::unix::net::UnixStream,
    expected_token: &str,
    tx_batches: &mpsc::Sender<Vec<PathBuf>>,
    tx_rpc: &mpsc::Sender<PendingIpcCall>,
    wake_ctx: &Option<egui::Context>,
    stop_flag: &AtomicBool,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let Ok(read_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_stream);
    while !stop_flag.load(Ordering::SeqCst) {
        let mut line = String::new();
        let read_res = {
            let mut limited = (&mut reader).take(MAX_IPC_LINE_BYTES);
            limited.read_line(&mut line)
        };
        match read_res {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = process_jsonrpc_line(trimmed, expected_token, tx_batches, tx_rpc, wake_ctx);
        if let Ok(mut encoded) = serde_json::to_string(&resp) {
            encoded.push('\n');
            if stream.write_all(encoded.as_bytes()).is_err() || stream.flush().is_err() {
                break;
            }
        }
    }
}

fn process_jsonrpc_line(
    line: &str,
    expected_token: &str,
    tx_batches: &mpsc::Sender<Vec<PathBuf>>,
    tx_rpc: &mpsc::Sender<PendingIpcCall>,
    wake_ctx: &Option<egui::Context>,
) -> JsonRpcResponse {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return JsonRpcResponse::err(
                serde_json::Value::Null,
                JSONRPC_PARSE_ERROR,
                format!("parse error: {e}"),
            );
        }
    };
    let id = req.id.clone().unwrap_or(serde_json::Value::Null);
    if req.jsonrpc != "2.0" {
        return JsonRpcResponse::err(id, JSONRPC_INVALID_REQUEST, "jsonrpc must be '2.0'");
    }

    let provided_token = req
        .token
        .as_deref()
        .or_else(|| req.params.get("token").and_then(serde_json::Value::as_str));
    if provided_token != Some(expected_token) {
        return JsonRpcResponse::err(id, JSONRPC_UNAUTHORIZED, "invalid or missing IPC token");
    }

    match req.method.as_str() {
        "studio.ping" => JsonRpcResponse::ok(
            id,
            serde_json::json!({
                "ok": true,
                "version": env!("CARGO_PKG_VERSION"),
            }),
        ),
        "app.open_files" => {
            let paths: Vec<PathBuf> = req
                .params
                .get("files")
                .and_then(serde_json::Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(serde_json::Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default();
            let count = paths.len();
            let _ = tx_batches.send(paths);
            if let Some(ref ctx) = wake_ctx {
                ctx.request_repaint();
            }
            JsonRpcResponse::ok(
                id,
                serde_json::json!({
                    "accepted": true,
                    "count": count,
                }),
            )
        }
        _ => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            if tx_rpc
                .send(PendingIpcCall {
                    request: req,
                    reply_tx,
                })
                .is_err()
            {
                return JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, "IPC receiver closed");
            }
            if let Some(ref ctx) = wake_ctx {
                ctx.request_repaint();
            }
            match reply_rx.recv_timeout(RPC_DISPATCH_TIMEOUT) {
                Ok(resp) => resp,
                Err(_) => JsonRpcResponse::err(
                    id,
                    JSONRPC_SERVER_ERROR,
                    "studio main loop timed out dispatching JSON-RPC",
                ),
            }
        }
    }
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn serve_spool_dir_loop(
    spool_dir: &Path,
    expected_token: &str,
    tx_batches: &mpsc::Sender<Vec<PathBuf>>,
    tx_rpc: &mpsc::Sender<PendingIpcCall>,
    wake_ctx: &Option<egui::Context>,
    stop_flag: &AtomicBool,
) {
    while !stop_flag.load(Ordering::SeqCst) {
        let mut handled_any = false;
        if let Ok(entries) = fs::read_dir(spool_dir) {
            for entry in entries.flatten() {
                if stop_flag.load(Ordering::SeqCst) {
                    return;
                }
                let req_path = entry.path();
                let Some(file_name) = req_path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Some(req_id) = file_name.strip_suffix(".req") else {
                    continue;
                };
                let mut raw_line = String::new();
                let read_ok = fs::File::open(&req_path).and_then(|f| {
                    f.take(MAX_IPC_LINE_BYTES)
                        .read_to_string(&mut raw_line)
                        .map(|_| ())
                });
                let _ = fs::remove_file(&req_path);
                if read_ok.is_err() {
                    continue;
                }
                handled_any = true;
                let resp = process_jsonrpc_line(
                    raw_line.trim(),
                    expected_token,
                    tx_batches,
                    tx_rpc,
                    wake_ctx,
                );
                if let Ok(encoded) = serde_json::to_string(&resp) {
                    let resp_tmp = spool_dir.join(format!("{req_id}.resp.tmp"));
                    let resp_final = spool_dir.join(format!("{req_id}.resp"));
                    if write_private_file(&resp_tmp, encoded.as_bytes()).is_ok()
                        && fs::rename(&resp_tmp, &resp_final).is_err()
                    {
                        let _ = write_private_file(&resp_final, encoded.as_bytes());
                        let _ = fs::remove_file(&resp_tmp);
                    }
                }
            }
        } else {
            break;
        }
        if !handled_any {
            thread::sleep(Duration::from_millis(4));
        }
    }
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn spool_dir_roundtrip(spool_dir: &Path, line: &str) -> Result<JsonRpcResponse, String> {
    if !spool_dir.is_dir() {
        return Err(format!(
            "IPC spool directory missing: {}",
            spool_dir.display()
        ));
    }
    let req_id = Uuid::new_v4().to_string();
    let req_tmp = spool_dir.join(format!("{req_id}.req.tmp"));
    let req_final = spool_dir.join(format!("{req_id}.req"));
    let resp_final = spool_dir.join(format!("{req_id}.resp"));

    write_private_file(&req_tmp, line.as_bytes())
        .map_err(|e| format!("IPC spool write failed ({}): {e}", req_tmp.display()))?;
    if fs::rename(&req_tmp, &req_final).is_err() {
        write_private_file(&req_final, line.as_bytes()).map_err(|e| {
            let _ = fs::remove_file(&req_tmp);
            format!("IPC spool commit failed ({}): {e}", req_final.display())
        })?;
        let _ = fs::remove_file(&req_tmp);
    }

    let deadline = Instant::now() + IO_TIMEOUT;
    while Instant::now() < deadline {
        if resp_final.exists() {
            let mut resp_line = String::new();
            let read_res = fs::File::open(&resp_final).and_then(|f| {
                f.take(MAX_IPC_LINE_BYTES)
                    .read_to_string(&mut resp_line)
                    .map(|_| ())
            });
            let _ = fs::remove_file(&resp_final);
            read_res
                .map_err(|e| format!("IPC spool read failed ({}): {e}", resp_final.display()))?;
            return serde_json::from_str::<JsonRpcResponse>(resp_line.trim())
                .map_err(|e| format!("invalid IPC spool JSON-RPC response: {e}"));
        }
        thread::sleep(Duration::from_millis(2));
    }

    let _ = fs::remove_file(&req_final);
    let _ = fs::remove_file(&resp_final);
    Err(format!(
        "IPC spool timed out after {:?} ({})",
        IO_TIMEOUT,
        spool_dir.display()
    ))
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

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut opts = fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);

    let mut file = opts.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn ensure_private_parent_dir(lock_path: &Path) -> Result<(), String> {
    let Some(parent) = lock_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    if !parent.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|e| format!("failed to create IPC dir {}: {e}", parent.display()))?;
        }
        #[cfg(not(unix))]
        {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create IPC dir {}: {e}", parent.display()))?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("failed to set 0o700 on IPC dir {}: {e}", parent.display()))?;
    }
    Ok(())
}

fn write_lockfile_atomic(lock_path: &Path, contents: &str) -> Result<(), String> {
    ensure_private_parent_dir(lock_path)?;
    let tmp = lock_path.with_extension(format!("lock.tmp.{}", std::process::id()));
    write_private_file(&tmp, contents.as_bytes())
        .map_err(|e| format!("failed to write temp lockfile {}: {e}", tmp.display()))?;
    if fs::rename(&tmp, lock_path).is_err() {
        write_private_file(lock_path, contents.as_bytes()).map_err(|e| {
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
    use coree_core::{BezierPath, Color, Fill, NodePayload, Rect2D, SceneObject};

    #[cfg(unix)]
    #[test]
    fn uds_socket_has_0700_dir_and_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let private_dir = dir.path().join("ipc_priv");
        let lock_path = private_dir.join("instance.lock");
        let server = IpcServer::bind_at(&lock_path).expect("bind UDS IPC server");

        let dir_mode = fs::metadata(&private_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);

        let lock_mode = fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(lock_mode, 0o600);

        let sock_mode = fs::metadata(server.socket_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(sock_mode, 0o600);
    }

    #[test]
    fn ipc_single_instance_uds_handoff_and_stale_cleanup_work() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("instance.lock");
        let pending_file = dir.path().join("تصميم").join("card.cdr");
        let arabic_cdr = dir.path().join("شعار").join("poster.cdr");
        let native_file = dir.path().join("badge.coree");

        assert!(!try_handoff_at(&lock_path, &[pending_file]));

        {
            let server = IpcServer::bind_at(&lock_path).expect("bind UDS IPC server");
            assert!(lock_path.exists());
            #[cfg(unix)]
            assert!(server.socket_path().exists());

            let handed = try_handoff_at(&lock_path, &[arabic_cdr.clone(), native_file.clone()]);
            assert!(handed, "live UDS IPC server must acknowledge handoff");

            let batches = server.try_recv_batches();
            assert_eq!(batches.len(), 1);
            assert_eq!(batches[0], vec![arabic_cdr, native_file]);

            assert!(try_handoff_at(&lock_path, &[]));
            let focus_batches = server.try_recv_batches();
            assert_eq!(focus_batches, vec![Vec::<PathBuf>::new()]);
        }

        assert!(!lock_path.exists());
        #[cfg(unix)]
        assert!(!lock_path.with_extension("sock").exists());

        // Stale lockfile left by a crashed process must return false and self-clean.
        let stale = serde_json::json!({
            "socket_path": dir.path().join("missing.sock").to_string_lossy(),
            "pipe_name": "",
            "spool_dir": dir.path().join("missing.spool").to_string_lossy(),
            "pid": 999_999u32,
            "token": "stale-token"
        });
        fs::write(&lock_path, stale.to_string()).expect("write stale lock");
        assert!(!try_handoff_at(&lock_path, &[]));
        assert!(!lock_path.exists(), "stale lockfile must be cleaned up");
    }

    #[test]
    fn spool_dir_transport_roundtrip_and_fast_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = dir.path().join("win_ipc.spool");
        fs::create_dir_all(&spool).expect("create spool");

        let (tx_batches, rx_batches) = mpsc::channel::<Vec<PathBuf>>();
        let (tx_rpc, _rx_rpc) = mpsc::channel::<PendingIpcCall>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let spool_bg = spool.clone();

        let srv = thread::spawn(move || {
            serve_spool_dir_loop(&spool_bg, "tok-123", &tx_batches, &tx_rpc, &None, &stop_bg);
        });

        let ping_line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "studio.ping",
            "params": {},
            "token": "tok-123"
        })
        .to_string();
        let resp = spool_dir_roundtrip(&spool, &ping_line).expect("spool ping roundtrip");
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["ok"], serde_json::json!(true));

        let open_line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "app.open_files",
            "params": { "files": ["شعار.cdr"] },
            "token": "tok-123"
        })
        .to_string();
        let open_resp = spool_dir_roundtrip(&spool, &open_line).expect("spool open_files");
        assert_eq!(open_resp.result.unwrap()["count"], serde_json::json!(1));
        assert_eq!(
            rx_batches.try_recv().unwrap(),
            vec![PathBuf::from("شعار.cdr")]
        );

        let bad_resp = spool_dir_roundtrip(&spool, "{not-json").expect("spool parse error");
        assert_eq!(bad_resp.error.unwrap().code, JSONRPC_PARSE_ERROR);

        let t0 = Instant::now();
        stop.store(true, Ordering::SeqCst);
        srv.join().expect("spool server joins cleanly");
        assert!(
            t0.elapsed() < Duration::from_millis(200),
            "spool shutdown must never hang"
        );
    }

    #[test]
    fn json_rpc_bidirectional_command_history_roundtrip_and_undo_redo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("rpc.lock");
        let server = Arc::new(IpcServer::bind_at(&lock_path).expect("bind IPC RPC server"));
        let done = Arc::new(AtomicBool::new(false));

        let server_bg = Arc::clone(&server);
        let done_bg = Arc::clone(&done);
        let worker = thread::spawn(move || {
            let mut doc = Document::new_a4("RPC Doc");
            let mut history = CommandHistory::default();
            while !done_bg.load(Ordering::SeqCst) {
                for call in server_bg.try_recv_rpc() {
                    let resp = dispatch_jsonrpc_on_session(&call.request, &mut doc, &mut history);
                    let _ = call.reply_tx.send(resp);
                }
                thread::sleep(Duration::from_millis(5));
            }
        });

        // 1. studio.ping
        let ping_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(1, "studio.ping", serde_json::json!({})),
        )
        .expect("studio.ping");
        assert!(ping_resp.error.is_none());
        assert_eq!(ping_resp.result.unwrap()["ok"], serde_json::json!(true));

        // 2. Unauthorized token rejection (-32001)
        let bad_tok_req = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(serde_json::json!(2)),
            method: "scene.get_document".to_owned(),
            params: serde_json::json!({}),
            token: Some("wrong-token".to_owned()),
        };
        let bad_tok_resp = send_jsonrpc_at(&lock_path, &bad_tok_req).expect("bad token response");
        assert_eq!(bad_tok_resp.error.unwrap().code, JSONRPC_UNAUTHORIZED);

        // 3. scene.execute_command (CreateObject)
        let obj = SceneObject::new(
            ObjectId(0),
            "RpcRect",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                10.0, 10.0, 30.0, 20.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(20, 160, 90, 255)));
        let create_op = CommandOp::create_object(obj, None);
        let create_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(
                3,
                "scene.execute_command",
                serde_json::json!({ "op": create_op }),
            ),
        )
        .expect("scene.execute_command");
        assert!(create_resp.error.is_none());
        assert_eq!(
            create_resp.result.unwrap()["object_count"],
            serde_json::json!(1)
        );

        // 4. scene.list_objects & scene.get_object
        let list_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(4, "scene.list_objects", serde_json::json!({})),
        )
        .expect("scene.list_objects");
        let objs = list_resp.result.unwrap()["objects"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(objs.len(), 1);
        let oid = objs[0]["id"].as_u64().unwrap();

        let get_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(5, "scene.get_object", serde_json::json!({ "id": oid })),
        )
        .expect("scene.get_object");
        assert_eq!(
            get_resp.result.unwrap()["name"],
            serde_json::json!("RpcRect")
        );

        // 5. scene.undo & scene.redo & scene.history
        let undo_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(6, "scene.undo", serde_json::json!({})),
        )
        .expect("scene.undo");
        assert_eq!(
            undo_resp.result.unwrap()["object_count"],
            serde_json::json!(0)
        );

        let redo_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(7, "scene.redo", serde_json::json!({})),
        )
        .expect("scene.redo");
        assert_eq!(
            redo_resp.result.unwrap()["object_count"],
            serde_json::json!(1)
        );

        let hist_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(8, "scene.history", serde_json::json!({})),
        )
        .expect("scene.history");
        assert_eq!(
            hist_resp.result.unwrap()["undo"].as_array().unwrap().len(),
            1
        );

        // 6. scene.query_cql, scene.run_macro, scene.preflight
        let cql_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(
                90,
                "scene.query_cql",
                serde_json::json!({ "query": "@name == 'RpcRect'" }),
            ),
        )
        .expect("scene.query_cql");
        assert_eq!(
            cql_resp.result.unwrap()["object_ids"],
            serde_json::json!([oid])
        );

        let macro_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(
                91,
                "scene.run_macro",
                serde_json::json!({ "script": "let s = doc.pages[0].shapes[0]; s.name = \"MacroNamed\";" }),
            ),
        )
        .expect("scene.run_macro");
        assert!(macro_resp.error.is_none(), "{:?}", macro_resp.error);

        let pf_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(92, "scene.preflight", serde_json::json!({})),
        )
        .expect("scene.preflight");
        assert!(pf_resp.result.unwrap().get("issues").is_some());

        // 7. Unknown method (-32601)
        let unknown_resp = send_jsonrpc_at(
            &lock_path,
            &JsonRpcRequest::new(9, "scene.nonexistent", serde_json::json!({})),
        )
        .expect("unknown method");
        assert_eq!(unknown_resp.error.unwrap().code, JSONRPC_METHOD_NOT_FOUND);

        // 8. Malformed JSON over transport returns -32700 Parse error
        #[cfg(unix)]
        {
            use std::os::unix::net::UnixStream;
            let mut raw_stream =
                UnixStream::connect(server.socket_path()).expect("connect raw UDS");
            raw_stream.write_all(b"{not-json\n").unwrap();
            raw_stream.flush().unwrap();
            let mut line = String::new();
            BufReader::new(raw_stream).read_line(&mut line).unwrap();
            let parse_err: JsonRpcResponse = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(parse_err.error.unwrap().code, JSONRPC_PARSE_ERROR);
        }
        #[cfg(windows)]
        {
            let parse_err =
                spool_dir_roundtrip(&server.spool_dir, "{not-json").expect("spool raw parse err");
            assert_eq!(parse_err.error.unwrap().code, JSONRPC_PARSE_ERROR);
        }

        done.store(true, Ordering::SeqCst);
        worker.join().unwrap();
    }
}
