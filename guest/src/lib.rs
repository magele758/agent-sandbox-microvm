use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;

const WORKSPACE: &str = "/workspace";
const MAX_OUTPUT: usize = 1024 * 1024;
const WAIT_LIMIT: Duration = Duration::from_secs(30);
const EXITED_KEEP: usize = 16;

#[derive(Clone)]
pub struct AppState {
    token: String,
    procs: Arc<Mutex<ProcTable>>,
    root: PathBuf,
}

struct ProcTable {
    running: HashMap<u32, String>,
    exited: VecDeque<ProcInfo>,
}

#[derive(Clone, Serialize)]
struct ProcInfo {
    pid: u32,
    cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exited: Option<bool>,
}

impl Default for ProcTable {
    fn default() -> Self {
        Self {
            running: HashMap::new(),
            exited: VecDeque::new(),
        }
    }
}

impl ProcTable {
    fn insert_running(&mut self, pid: u32, cmd: String) {
        self.exited.retain(|p| p.pid != pid);
        self.running.insert(pid, cmd);
    }

    fn mark_exited(&mut self, pid: u32, cmd: String) {
        self.running.remove(&pid);
        self.exited.retain(|p| p.pid != pid);
        self.exited.push_back(ProcInfo {
            pid,
            cmd,
            exited: Some(true),
        });
        while self.exited.len() > EXITED_KEEP {
            self.exited.pop_front();
        }
    }

    fn list(&self) -> Vec<ProcInfo> {
        let mut out: Vec<ProcInfo> = self
            .running
            .iter()
            .map(|(pid, cmd)| ProcInfo {
                pid: *pid,
                cmd: cmd.clone(),
                exited: None,
            })
            .collect();
        out.sort_by_key(|p| p.pid);
        out.extend(self.exited.iter().cloned());
        out
    }

    fn running_cmd(&self, pid: u32) -> Option<String> {
        self.running.get(&pid).cloned()
    }
}

pub fn router(token: String) -> Router {
    router_with_root(token, PathBuf::from(WORKSPACE))
}

pub fn router_with_root(token: String, root: PathBuf) -> Router {
    let _ = std::fs::create_dir_all(&root);
    let state = AppState {
        token,
        procs: Arc::new(Mutex::new(ProcTable::default())),
        root,
    };
    Router::new()
        .route("/health", get(health))
        .route("/process.Process/Start", post(start))
        .route("/process.Process/List", post(list))
        .route("/process.Process/SendSignal", post(send_signal))
        .route("/filesystem.Filesystem/List", post(fs_list))
        .route("/filesystem.Filesystem/Write", post(fs_write))
        .route("/filesystem.Filesystem/Read", post(fs_read))
        .route("/init", any(not_found))
        .route("/upgrade", any(not_found))
        .route("/collapse", any(not_found))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
        .with_state(state)
}

async fn require_token(
    State(st): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if req.method() == axum::http::Method::GET && req.uri().path() == "/health" {
        return next.run(req).await;
    }
    if !token_ok(&st.token, req.headers()) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    next.run(req).await
}

fn token_ok(expected: &str, headers: &HeaderMap) -> bool {
    match headers.get("x-access-token").and_then(|v| v.to_str().ok()) {
        Some(got) => got == expected,
        None => false,
    }
}

fn err(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({"code": status.as_u16(), "message": message})),
    )
        .into_response()
}

async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn not_found() -> Response {
    err(StatusCode::NOT_FOUND, "not found")
}

#[derive(Deserialize)]
struct StartReq {
    process: ProcessSpec,
}

#[derive(Deserialize)]
struct ProcessSpec {
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    envs: HashMap<String, String>,
}

#[derive(Serialize)]
struct StartResp {
    event: StartEvent,
}

#[derive(Serialize)]
struct StartEvent {
    start: StartPid,
    data: StartData,
    end: StartEnd,
}

#[derive(Serialize)]
struct StartPid {
    pid: u32,
}

#[derive(Serialize)]
struct StartData {
    stdout: String,
    stderr: String,
}

#[derive(Serialize)]
struct StartEnd {
    #[serde(rename = "exitCode")]
    exit_code: i32,
}

async fn start(State(st): State<AppState>, Json(body): Json<StartReq>) -> Response {
    if body.process.cmd.is_empty() {
        return err(StatusCode::BAD_REQUEST, "cmd required");
    }
    let procs = st.procs.clone();
    let spec = body.process;
    let joined = tokio::task::spawn_blocking(move || run_start(procs, spec)).await;
    match joined {
        Ok(Ok(done)) => (
            StatusCode::OK,
            Json(StartResp {
                event: StartEvent {
                    start: StartPid { pid: done.pid },
                    data: StartData {
                        stdout: STANDARD.encode(&done.stdout),
                        stderr: STANDARD.encode(&done.stderr),
                    },
                    end: StartEnd {
                        exit_code: done.exit_code,
                    },
                },
            }),
        )
            .into_response(),
        Ok(Err(StartError::Timeout)) => err(StatusCode::REQUEST_TIMEOUT, "timeout"),
        Ok(Err(StartError::Spawn(msg))) => err(StatusCode::BAD_REQUEST, &msg),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "start failed"),
    }
}

struct Done {
    pid: u32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: i32,
}

enum StartError {
    Timeout,
    Spawn(String),
}

fn run_start(procs: Arc<Mutex<ProcTable>>, spec: ProcessSpec) -> Result<Done, StartError> {
    let mut command = Command::new(&spec.cmd);
    command.args(&spec.args);
    for (k, v) in &spec.envs {
        command.env(k, v);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| StartError::Spawn(e.to_string()))?;
    let pid = child.id();
    {
        let mut table = procs.lock().expect("proc table");
        table.insert_running(pid, spec.cmd.clone());
    }
    let stdout = child.stdout.take().expect("stdout pipe");
    let stderr = child.stderr.take().expect("stderr pipe");
    let stop = Arc::new(AtomicBool::new(false));
    let out_stop = stop.clone();
    let err_stop = stop.clone();
    let out_handle = std::thread::spawn(move || read_capped(stdout, out_stop));
    let err_handle = std::thread::spawn(move || read_capped(stderr, err_stop));

    let deadline = Instant::now() + WAIT_LIMIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                stop.store(true, Ordering::SeqCst);
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_handle.join();
                let _ = err_handle.join();
                let mut table = procs.lock().expect("proc table");
                table.mark_exited(pid, spec.cmd);
                return Err(StartError::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                stop.store(true, Ordering::SeqCst);
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_handle.join();
                let _ = err_handle.join();
                let mut table = procs.lock().expect("proc table");
                table.mark_exited(pid, spec.cmd);
                return Err(StartError::Spawn(e.to_string()));
            }
        }
    };
    stop.store(true, Ordering::SeqCst);
    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    let exit_code = status.code().unwrap_or(-1);
    {
        let mut table = procs.lock().expect("proc table");
        table.mark_exited(pid, spec.cmd);
    }
    Ok(Done {
        pid,
        stdout,
        stderr,
        exit_code,
    })
}

fn read_capped(mut reader: impl Read, stop: Arc<AtomicBool>) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        if stop.load(Ordering::SeqCst) {
            // Still drain until EOF so a killed child unblocks us via a closed pipe.
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let room = MAX_OUTPUT.saturating_sub(kept.len());
                if room > 0 {
                    let take = n.min(room);
                    kept.extend_from_slice(&buf[..take]);
                }
            }
            Err(_) => break,
        }
    }
    kept
}

async fn list(State(st): State<AppState>) -> impl IntoResponse {
    let table = st.procs.lock().expect("proc table");
    Json(json!({"processes": table.list()}))
}

#[derive(Deserialize)]
struct SignalReq {
    pid: u32,
    signal: i32,
}

async fn send_signal(State(st): State<AppState>, Json(body): Json<SignalReq>) -> Response {
    if body.pid == 0 || !(0..=64).contains(&body.signal) {
        return err(StatusCode::BAD_REQUEST, "invalid signal");
    }
    let known = {
        let table = st.procs.lock().expect("proc table");
        table.running_cmd(body.pid).is_some()
    };
    if !known {
        return err(StatusCode::NOT_FOUND, "not found");
    }
    let rc = unsafe { libc::kill(body.pid as i32, body.signal) };
    if rc == 0 {
        return Json(json!({})).into_response();
    }
    let errno = std::io::Error::last_os_error();
    if errno.raw_os_error() == Some(libc::ESRCH) {
        err(StatusCode::NOT_FOUND, "not found")
    } else {
        err(StatusCode::BAD_REQUEST, "signal failed")
    }
}

#[derive(Deserialize)]
struct PathReq {
    path: String,
}

#[derive(Deserialize)]
struct WriteReq {
    path: String,
    data: String,
}

async fn fs_list(State(st): State<AppState>, Json(body): Json<PathReq>) -> Response {
    let path = match secure_path(&st.root, &body.path) {
        Ok(p) => p,
        Err(msg) => return err(StatusCode::BAD_REQUEST, msg),
    };
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(_) => return err(StatusCode::NOT_FOUND, "not found"),
    };
    if !meta.is_dir() {
        return err(StatusCode::BAD_REQUEST, "not a directory");
    }
    let rd = match std::fs::read_dir(&path) {
        Ok(rd) => rd,
        Err(_) => return err(StatusCode::BAD_REQUEST, "list failed"),
    };
    let mut entries = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        let kind = match ent.file_type() {
            Ok(t) if t.is_symlink() => "symlink",
            Ok(t) if t.is_dir() => "dir",
            _ => "file",
        };
        entries.push(json!({"name": name, "type": kind}));
    }
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Json(json!({"entries": entries})).into_response()
}

async fn fs_write(State(st): State<AppState>, Json(body): Json<WriteReq>) -> Response {
    let path = match secure_path(&st.root, &body.path) {
        Ok(p) => p,
        Err(msg) => return err(StatusCode::BAD_REQUEST, msg),
    };
    let bytes = match STANDARD.decode(body.data.as_bytes()) {
        Ok(b) => b,
        Err(_) => return err(StatusCode::BAD_REQUEST, "invalid base64"),
    };
    if let Some(parent) = path.parent() {
        if let Err(msg) = secure_path(&st.root, &parent.to_string_lossy()) {
            return err(StatusCode::BAD_REQUEST, msg);
        }
        if std::fs::create_dir_all(parent).is_err() {
            return err(StatusCode::BAD_REQUEST, "mkdir failed");
        }
    }
    match std::fs::write(&path, bytes) {
        Ok(()) => Json(json!({})).into_response(),
        Err(_) => err(StatusCode::BAD_REQUEST, "write failed"),
    }
}

async fn fs_read(State(st): State<AppState>, Json(body): Json<PathReq>) -> Response {
    let path = match secure_path(&st.root, &body.path) {
        Ok(p) => p,
        Err(msg) => return err(StatusCode::BAD_REQUEST, msg),
    };
    match std::fs::read(&path) {
        Ok(bytes) => Json(json!({"data": STANDARD.encode(bytes)})).into_response(),
        Err(_) => err(StatusCode::NOT_FOUND, "not found"),
    }
}

fn secure_path(root: &Path, raw: &str) -> Result<PathBuf, &'static str> {
    if raw.is_empty() || raw.contains('\0') {
        return Err("invalid path");
    }
    let input = Path::new(raw);
    if input.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path escape");
    }
    if raw.split(['/', '\\']).any(|seg| seg == "..") {
        return Err("path escape");
    }
    let combined = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };
    let mut lexical = PathBuf::new();
    for c in combined.components() {
        match c {
            Component::Prefix(_) => return Err("invalid path"),
            Component::RootDir => lexical.push("/"),
            Component::CurDir => {}
            Component::ParentDir => return Err("path escape"),
            Component::Normal(seg) => lexical.push(seg),
        }
    }
    if lexical != root && !lexical.starts_with(root) {
        return Err("outside workspace");
    }
    let canon_root = root.canonicalize().map_err(|_| "workspace missing")?;
    let rel = lexical.strip_prefix(root).map_err(|_| "outside workspace")?;
    let mut current = canon_root.clone();
    let comps: Vec<Component> = rel.components().collect();
    if comps.is_empty() {
        return Ok(current);
    }
    for (i, comp) in comps.iter().enumerate() {
        let next = current.join(comp);
        let last = i + 1 == comps.len();
        match std::fs::symlink_metadata(&next) {
            Err(_) if last => return Ok(next),
            Err(_) => return Err("not found"),
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&next).map_err(|_| "symlink escape")?;
                let resolved = if target.is_absolute() {
                    target
                } else {
                    current.join(target)
                };
                if resolved.components().any(|c| matches!(c, Component::ParentDir)) {
                    let canon = resolved.canonicalize().map_err(|_| "symlink escape")?;
                    if canon != canon_root && !canon.starts_with(&canon_root) {
                        return Err("symlink escape");
                    }
                    current = canon;
                } else {
                    let canon = resolved.canonicalize().map_err(|_| "symlink escape")?;
                    if canon != canon_root && !canon.starts_with(&canon_root) {
                        return Err("symlink escape");
                    }
                    current = canon;
                }
            }
            Ok(_) => current = next,
        }
    }
    if current.exists() {
        let canon = current.canonicalize().map_err(|_| "symlink escape")?;
        if canon != canon_root && !canon.starts_with(&canon_root) {
            return Err("symlink escape");
        }
        return Ok(canon);
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app() -> Router {
        router("secret".into())
    }

    async fn body_json(res: Response) -> Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn missing_token() {
        let res = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/process.Process/List")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let v = body_json(res).await;
        assert_eq!(v["code"], 401);
        assert_eq!(v["message"], "unauthorized");
    }

    #[tokio::test]
    async fn echo_stdout() {
        let res = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/process.Process/Start")
                    .header("content-type", "application/json")
                    .header("x-access-token", "secret")
                    .body(Body::from(
                        r#"{"process":{"cmd":"/bin/echo","args":["hi"],"envs":{"A":"b"}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        let stdout = STANDARD
            .decode(v["event"]["data"]["stdout"].as_str().unwrap())
            .unwrap();
        assert_eq!(stdout, b"hi\n");
        assert_eq!(v["event"]["end"]["exitCode"], 0);
        assert!(v["event"]["start"]["pid"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn path_escape() {
        let res = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/filesystem.Filesystem/Read")
                    .header("content-type", "application/json")
                    .header("x-access-token", "secret")
                    .body(Body::from(r#"{"path":"/workspace/../etc/passwd"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn init_not_found() {
        let res = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/init")
                    .header("x-access-token", "secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
