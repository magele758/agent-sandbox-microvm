use crate::firecracker::{boot_calls, jailer_argv, ApiCall, BootParams};
use crate::kvm;
use std::path::PathBuf;
use std::process::Child;

#[cfg(target_os = "linux")]
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum BootError {
    KvmUnavailable,
    MissingArtifact(&'static str),
    Spawn(std::io::Error),
    Api { path: &'static str, detail: String },
}

pub struct BootedVm {
    pub child: Child,
}

pub struct BootArtifacts {
    pub jailer_bin: PathBuf,
    pub firecracker: PathBuf,
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
}

impl BootArtifacts {
    pub fn from_env() -> Result<Self, BootError> {
        fn required(key: &'static str) -> Result<PathBuf, BootError> {
            std::env::var_os(key)
                .map(PathBuf::from)
                .ok_or(BootError::MissingArtifact(key))
        }
        Ok(Self {
            jailer_bin: required("SANDBOX_JAILER")?,
            firecracker: required("SANDBOX_FIRECRACKER")?,
            kernel: required("SANDBOX_KERNEL")?,
            rootfs: required("SANDBOX_ROOTFS")?,
        })
    }
}

/// Applies the ordered API calls only when Linux, `/dev/kvm`, and `SANDBOX_ENABLE_KVM=1`.
pub fn boot_sandbox(
    sandbox_id: &str,
    vcpu: u32,
    mem_mib: u32,
    tap_name: &str,
    guest_mac: &str,
) -> Result<BootedVm, BootError> {
    if !kvm::execution_allowed() {
        return Err(BootError::KvmUnavailable);
    }
    let artifacts = BootArtifacts::from_env()?;
    let calls = boot_calls(&BootParams {
        vcpu,
        mem_mib,
        rootfs_path: &artifacts.rootfs.display().to_string(),
        tap_name,
        guest_mac,
        kernel_path: &artifacts.kernel.display().to_string(),
    });
    let argv = jailer_argv(sandbox_id, &artifacts.firecracker.display().to_string());
    start_and_configure(sandbox_id, &artifacts.jailer_bin, &argv, &calls)
}

fn start_and_configure(
    sandbox_id: &str,
    jailer_bin: &std::path::Path,
    argv: &[String],
    calls: &[ApiCall],
) -> Result<BootedVm, BootError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (sandbox_id, jailer_bin, argv, calls);
        return Err(BootError::KvmUnavailable);
    }
    #[cfg(target_os = "linux")]
    {
        linux_start(sandbox_id, jailer_bin, argv, calls)
    }
}

#[cfg(target_os = "linux")]
fn linux_start(
    sandbox_id: &str,
    jailer_bin: &std::path::Path,
    argv: &[String],
    calls: &[ApiCall],
) -> Result<BootedVm, BootError> {
    use std::os::unix::net::UnixStream;

    let api_sock = format!("/srv/jailer/firecracker/{sandbox_id}/root/run/firecracker.socket");
    let mut cmd = Command::new(jailer_bin);
    cmd.args(&argv[1..])
        .arg("--")
        .arg("--api-sock")
        .arg("/run/firecracker.socket")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(BootError::Spawn)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_err = String::from("api socket not ready");
    loop {
        if let Some(status) = child.try_wait().map_err(BootError::Spawn)? {
            return Err(BootError::Api {
                path: "/",
                detail: format!("jailer exited {status}"),
            });
        }
        match UnixStream::connect(&api_sock) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .map_err(BootError::Spawn)?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .map_err(BootError::Spawn)?;
                for call in calls {
                    let body = serde_json::to_string(&call.body).expect("body");
                    if let Err(err) = put_json(&mut stream, call.path, &body) {
                        let _ = child.kill();
                        return Err(BootError::Api {
                            path: call.path,
                            detail: err,
                        });
                    }
                }
                return Ok(BootedVm { child });
            }
            Err(err) => last_err = err.to_string(),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(BootError::Api {
                path: "/",
                detail: last_err,
            });
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(target_os = "linux")]
fn put_json(stream: &mut std::os::unix::net::UnixStream, path: &str, body: &str) -> Result<(), String> {
    let req = format!(
        "PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let status = text
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("bad response: {text}"))?;
    let code: u16 = status.parse().map_err(|_| format!("bad status: {text}"))?;
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(format!("HTTP {code}: {text}"))
    }
}
