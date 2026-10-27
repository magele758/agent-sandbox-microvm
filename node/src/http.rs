use crate::boot::{self, BootError, BootedVm};
use crate::kvm;
use crate::slots::{SlotError, SlotPool};
use crate::state::{RunningSandbox, SandboxTable};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::Ipv4Addr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct AppState {
    inner: std::sync::Arc<Mutex<Inner>>,
}

struct Inner {
    slots: SlotPool,
    sandboxes: SandboxTable,
    vms: std::collections::HashMap<String, BootedVm>,
    cpu_millis: i64,
    mem_mib: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateSandbox {
    #[serde(rename = "sandboxID")]
    pub sandbox_id: String,
    #[serde(rename = "templateID")]
    pub template_id: String,
    pub vcpu: u32,
    #[serde(rename = "memMiB")]
    pub mem_mib: i64,
    #[serde(default, rename = "envVars")]
    pub env_vars: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_true", rename = "allowInternet")]
    pub allow_internet: bool,
    #[serde(default, rename = "envdAccessToken")]
    pub envd_access_token: String,
}
fn default_true() -> bool {
    true
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    kvm: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capacity {
    cpu_millis: i64,
    mem_mi_b: i64,
    committed_mem_mi_b: i64,
    running: i64,
    ready: bool,
}

#[derive(Serialize)]
struct Created {
    #[serde(rename = "sandboxID")]
    sandbox_id: String,
    state: &'static str,
    #[serde(rename = "guestURL")]
    guest_url: String,
}

pub fn router() -> Router {
    let state = AppState::new();
    router_with(state)
}

pub fn router_with(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/capacity", get(capacity))
        .route("/v1/sandboxes", post(create_sandbox))
        .route("/v1/sandboxes/:id", axum::routing::delete(delete_sandbox))
        .with_state(state)
}

impl AppState {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(Inner {
                slots: SlotPool::new(),
                sandboxes: SandboxTable::default(),
                vms: std::collections::HashMap::new(),
                cpu_millis: host_cpu_millis(),
                mem_mib: host_mem_mib(),
            })),
        }
    }
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        kvm: kvm::kvm_device_present(),
    })
}

async fn capacity(State(state): State<AppState>) -> Json<Capacity> {
    let inner = state.inner.lock().expect("state");
    Json(Capacity {
        cpu_millis: inner.cpu_millis,
        mem_mi_b: inner.mem_mib,
        committed_mem_mi_b: inner.sandboxes.committed_mem_mib(),
        running: inner.sandboxes.running_count(),
        ready: kvm::kvm_device_present(),
    })
}

async fn create_sandbox(
    State(state): State<AppState>,
    Json(body): Json<CreateSandbox>,
) -> Result<Json<Created>, (StatusCode, Json<Value>)> {
    if body.sandbox_id.is_empty() || body.template_id.is_empty() || body.vcpu == 0 || body.mem_mib <= 0
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"code": 400, "message": "invalid sandbox spec"})),
        ));
    }

    if !kvm::execution_allowed() {
        return Err(kvm_unavailable());
    }

    let slot = {
        let mut inner = state.inner.lock().expect("state");
        if inner.sandboxes.contains(&body.sandbox_id) {
            return Err((
                StatusCode::CONFLICT,
                Json(json!({"code": 409, "message": "sandbox exists"})),
            ));
        }
        match inner.slots.allocate(&body.sandbox_id) {
            Ok(slot) => slot,
            Err(SlotError::Exhausted) => {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "code": 503,
                        "message": "no free slot",
                        "error_code": "sandbox_create_refused"
                    })),
                ));
            }
            Err(SlotError::Unknown) => unreachable!("allocate does not return unknown"),
        }
    };

    // envVars / allowInternet: persist into guest via a small marker file on the
    // per-sandbox rootfs is M1+; for M0 we stash on the running record and pass
    // token through guestURL auth later. Keep values so they are not dropped.
    let _env_vars = body.env_vars.clone();
    let _allow_internet = body.allow_internet;
    let _token = body.envd_access_token.clone();

    let booted = tokio::task::spawn_blocking({
        let id = body.sandbox_id.clone();
        let vcpu = body.vcpu;
        let mem = body.mem_mib as u32;
        let tap = slot.tap.clone();
        let mac = slot.mac.clone();
        let ip = slot.ip;
        move || boot::boot_sandbox(&id, vcpu, mem, &tap, &mac, ip)
    })
    .await
    .expect("boot task");

    match booted {
        Ok(mut vm) => {
            // Product rule: running means guest-agent /health answers.
            let guest_url = format!("http://{}:49983", slot.ip);
            let health_ok = tokio::task::spawn_blocking({
                let url = format!("{guest_url}/health");
                move || wait_guest_health(&url, Duration::from_secs(90))
            })
            .await
            .expect("health task");
            if let Err(err) = health_ok {
                boot::teardown_vm(&mut vm);
                let mut inner = state.inner.lock().expect("state");
                let _ = inner.slots.release(&body.sandbox_id);
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "code": 500,
                        "message": format!("sandbox start failed: guest health: {err}")
                    })),
                ));
            }
            // Best-effort push of env vars into the guest (no-op if endpoint rejects).
            let _ = tokio::task::spawn_blocking({
                let base = guest_url.clone();
                let env = _env_vars;
                let allow = _allow_internet;
                let token = _token;
                move || push_guest_init(&base, &env, allow, &token)
            })
            .await;
            let mut inner = state.inner.lock().expect("state");
            inner.vms.insert(body.sandbox_id.clone(), vm);
            inner.sandboxes.insert(
                body.sandbox_id.clone(),
                RunningSandbox {
                    mem_mib: body.mem_mib,
                    guest_ip: slot.ip,
                    env_vars: body.env_vars,
                    allow_internet: body.allow_internet,
                },
            );
            Ok(Json(created(&body.sandbox_id, slot.ip)))
        }
        Err(BootError::KvmUnavailable) => {
            let mut inner = state.inner.lock().expect("state");
            let _ = inner.slots.release(&body.sandbox_id);
            Err(kvm_unavailable())
        }
        Err(err) => {
            let mut inner = state.inner.lock().expect("state");
            let _ = inner.slots.release(&body.sandbox_id);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "code": 500,
                    "message": format!("sandbox start failed: {err}")
                })),
            ))
        }
    }
}

async fn delete_sandbox(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mut inner = state.inner.lock().expect("state");
    if inner.sandboxes.remove(&id).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"code": 404, "message": "not found"})),
        ));
    }
    if let Some(mut vm) = inner.vms.remove(&id) {
        boot::teardown_vm(&mut vm);
    }
    match inner.slots.release(&id) {
        Ok(_) | Err(SlotError::Unknown) => {}
        Err(SlotError::Exhausted) => {}
    }
    Ok(StatusCode::NO_CONTENT)
}

fn created(id: &str, ip: Ipv4Addr) -> Created {
    Created {
        sandbox_id: id.to_string(),
        state: "running",
        guest_url: format!("http://{ip}:49983"),
    }
}

fn kvm_unavailable() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "code": 503,
            "message": "kvm unavailable",
            "error_code": "sandbox_create_refused"
        })),
    )
}

fn wait_guest_health(url: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("not tried");
    while Instant::now() < deadline {
        match curl_get(url) {
            Ok(body) if body.contains("ok") => return Ok(()),
            Ok(body) => last = format!("unexpected body: {body}"),
            Err(err) => last = err,
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(last)
}

fn curl_get(url: &str) -> Result<String, String> {
    let output = std::process::Command::new("curl")
        .args(["-sf", "--connect-timeout", "1", "--max-time", "2", url])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!("curl exit {}", output.status))
    }
}

fn push_guest_init(
    guest_url: &str,
    env_vars: &std::collections::BTreeMap<String, String>,
    allow_internet: bool,
    token: &str,
) {
    // Write a JSON blob into /workspace/.sandbox-init via filesystem API if available.
    let payload = json!({
        "envVars": env_vars,
        "allowInternet": allow_internet,
        "envdAccessToken": token,
    });
    let body = json!({
        "path": "/.sandbox-init.json",
        "data": base64_encode(payload.to_string().as_bytes()),
    });
    let _ = std::process::Command::new("curl")
        .args([
            "-sf",
            "--connect-timeout",
            "2",
            "-X",
            "POST",
            &format!("{guest_url}/filesystem.Filesystem/Write"),
            "-H",
            "content-type: application/json",
            "-d",
            &body.to_string(),
        ])
        .status();
}

fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let c = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (a << 16) | (b << 8) | c;
        out.push(T[((triple >> 18) & 63) as usize] as char);
        out.push(T[((triple >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[((triple >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(T[(triple & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootError::KvmUnavailable => write!(f, "kvm unavailable"),
            BootError::MissingArtifact(key) => write!(f, "missing {key}"),
            BootError::Spawn(err) => write!(f, "spawn: {err}"),
            BootError::Api { path, detail } => write!(f, "{path}: {detail}"),
            BootError::Tap(err) => write!(f, "tap: {err}"),
            BootError::Stage(err) => write!(f, "stage: {err}"),
        }
    }
}

fn host_cpu_millis() -> i64 {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as i64)
        .unwrap_or(1);
    cpus.saturating_mul(1000)
}

fn host_mem_mib() -> i64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    let kb: i64 = rest.split_whitespace().next().unwrap_or("0").parse().unwrap_or(0);
                    return kb / 1024;
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                if let Ok(bytes) = text.trim().parse::<i64>() {
                    return bytes / (1024 * 1024);
                }
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn create_req() -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/sandboxes")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"sandboxID":"sbx_abc","templateID":"base","vcpu":2,"memMiB":2048,"envVars":{"FOO":"bar"},"allowInternet":true,"envdAccessToken":"token"}"#,
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn health_reports_kvm_flag() {
        let app = router();
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["kvm"], kvm::kvm_device_present());
    }

    #[tokio::test]
    async fn create_without_kvm_is_503_and_does_not_commit_memory() {
        // Clear the enable flag for this test so presence of /dev/kvm does not fail the suite.
        let prev = std::env::var_os("SANDBOX_ENABLE_KVM");
        std::env::remove_var("SANDBOX_ENABLE_KVM");
        assert!(
            !kvm::execution_allowed(),
            "gate must refuse create when SANDBOX_ENABLE_KVM is unset"
        );
        let state = AppState::new();
        let app = router_with(state.clone());
        let resp = app.oneshot(create_req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        assert_eq!(json["code"], 503);
        assert_eq!(json["message"], "kvm unavailable");
        assert_eq!(json["error_code"], "sandbox_create_refused");

        let app = router_with(state.clone());
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/capacity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["committedMemMiB"], 0);
        assert_eq!(json["running"], 0);
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.slots.free_count(), crate::slots::GUEST_SLOTS);
        match prev {
            Some(v) => std::env::set_var("SANDBOX_ENABLE_KVM", v),
            None => std::env::remove_var("SANDBOX_ENABLE_KVM"),
        }
    }

    #[tokio::test]
    async fn delete_unknown_is_404() {
        let app = router();
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri("/v1/sandboxes/sbx_missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_json(resp).await;
        assert_eq!(json["code"], 404);
    }
}
