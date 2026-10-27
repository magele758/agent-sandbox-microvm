use crate::firecracker::{boot_calls, jailer_argv, jailer_id, ApiCall, BootParams};
use crate::kvm;
use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum BootError {
    KvmUnavailable,
    MissingArtifact(&'static str),
    Spawn(std::io::Error),
    Api { path: &'static str, detail: String },
    Tap(String),
    Stage(String),
}

pub struct BootedVm {
    pub child: Child,
    pub jail_id: String,
    pub tap_name: String,
    pub chroot_base: PathBuf,
    pub work_rootfs: PathBuf,
}

pub struct BootArtifacts {
    pub jailer_bin: PathBuf,
    pub firecracker: PathBuf,
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
    pub chroot_base: PathBuf,
    pub priv_bin: Option<PathBuf>,
}

impl BootArtifacts {
    pub fn from_env() -> Result<Self, BootError> {
        fn required(key: &'static str) -> Result<PathBuf, BootError> {
            std::env::var_os(key)
                .map(PathBuf::from)
                .ok_or(BootError::MissingArtifact(key))
        }
        let chroot_base = std::env::var_os("SANDBOX_CHROOT_BASE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/sbxj"));
        let priv_bin = std::env::var_os("SANDBOX_PRIV_BIN").map(PathBuf::from).or_else(|| {
            let candidate = required("SANDBOX_JAILER")
                .ok()
                .and_then(|j| j.parent().map(|p| p.join("priv")));
            candidate.filter(|p| p.exists())
        });
        Ok(Self {
            jailer_bin: required("SANDBOX_JAILER")?,
            firecracker: required("SANDBOX_FIRECRACKER")?,
            kernel: required("SANDBOX_KERNEL")?,
            rootfs: required("SANDBOX_ROOTFS")?,
            chroot_base,
            priv_bin,
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
    guest_ip: Ipv4Addr,
) -> Result<BootedVm, BootError> {
    if !kvm::execution_allowed() {
        return Err(BootError::KvmUnavailable);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (sandbox_id, vcpu, mem_mib, tap_name, guest_mac, guest_ip);
        return Err(BootError::KvmUnavailable);
    }
    #[cfg(target_os = "linux")]
    {
        linux_boot(sandbox_id, vcpu, mem_mib, tap_name, guest_mac, guest_ip)
    }
}

#[cfg(target_os = "linux")]
fn linux_boot(
    sandbox_id: &str,
    vcpu: u32,
    mem_mib: u32,
    tap_name: &str,
    guest_mac: &str,
    guest_ip: Ipv4Addr,
) -> Result<BootedVm, BootError> {
    let artifacts = BootArtifacts::from_env()?;
    let jail_id = jailer_id(sandbox_id);
    std::fs::create_dir_all(&artifacts.chroot_base).map_err(|e| BootError::Stage(e.to_string()))?;
    ensure_image_cache(&artifacts)?;
    create_tap(&artifacts, tap_name)?;

    let uid = std::env::var("SANDBOX_JAILER_UID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let gid = std::env::var("SANDBOX_JAILER_GID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    let argv = jailer_argv(
        &jail_id,
        &artifacts.firecracker.display().to_string(),
        &artifacts.chroot_base.display().to_string(),
        uid,
        gid,
    );
    let mut cmd = Command::new(&artifacts.jailer_bin);
    cmd.args(&argv[1..])
        .arg("--")
        .arg("--api-sock")
        .arg("/run/firecracker.socket")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(BootError::Spawn)?;

    let api_sock = artifacts
        .chroot_base
        .join("firecracker")
        .join(&jail_id)
        .join("root/run/firecracker.socket");
    let jail_root = artifacts
        .chroot_base
        .join("firecracker")
        .join(&jail_id)
        .join("root");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_err = String::from("api socket not ready");
    loop {
        if let Some(status) = child.try_wait().map_err(BootError::Spawn)? {
            destroy_tap(&artifacts, tap_name);
            return Err(BootError::Api {
                path: "/",
                detail: format!("jailer exited {status}"),
            });
        }
        if api_sock.exists() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            destroy_tap(&artifacts, tap_name);
            return Err(BootError::Api {
                path: "/",
                detail: last_err,
            });
        }
        last_err = format!("waiting for {}", api_sock.display());
        std::thread::sleep(Duration::from_millis(20));
    }

    let work_rootfs = stage_into_jail(&artifacts, &jail_root)?;
    let guest_ip_s = guest_ip.to_string();
    let calls = boot_calls(&BootParams {
        vcpu,
        mem_mib,
        rootfs_path: "/rootfs.ext4",
        tap_name,
        guest_mac,
        kernel_path: "/vmlinux",
        guest_ip: &guest_ip_s,
        enable_net: true,
    });

    if let Err(err) = configure_vm(&api_sock, &calls) {
        let _ = child.kill();
        let _ = child.wait();
        destroy_tap(&artifacts, tap_name);
        cleanup_jail(&artifacts.chroot_base, &jail_id);
        return Err(err);
    }

    Ok(BootedVm {
        child,
        jail_id,
        tap_name: tap_name.to_string(),
        chroot_base: artifacts.chroot_base.clone(),
        work_rootfs,
    })
}

#[cfg(target_os = "linux")]
fn ensure_image_cache(artifacts: &BootArtifacts) -> Result<(), BootError> {
    let images = artifacts.chroot_base.join("images");
    std::fs::create_dir_all(&images).map_err(|e| BootError::Stage(e.to_string()))?;
    let cached_root = images.join("rootfs.ext4");
    let cached_kernel = images.join("vmlinux");
    if !cached_root.exists() {
        copy_best(&artifacts.rootfs, &cached_root)?;
    }
    if !cached_kernel.exists() {
        copy_best(&artifacts.kernel, &cached_kernel)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn stage_into_jail(artifacts: &BootArtifacts, jail_root: &Path) -> Result<PathBuf, BootError> {
    let images = artifacts.chroot_base.join("images");
    let dst_root = jail_root.join("rootfs.ext4");
    let dst_kernel = jail_root.join("vmlinux");
    // Per-sandbox writable rootfs via reflink/copy from cache.
    if dst_root.exists() {
        let _ = std::fs::remove_file(&dst_root);
    }
    copy_best(&images.join("rootfs.ext4"), &dst_root)?;
    if !dst_kernel.exists() {
        if std::fs::hard_link(images.join("vmlinux"), &dst_kernel).is_err() {
            copy_best(&images.join("vmlinux"), &dst_kernel)?;
        }
    }
    Ok(dst_root)
}

#[cfg(target_os = "linux")]
fn copy_best(src: &Path, dst: &Path) -> Result<(), BootError> {
    // Prefer cp --reflink=auto for speed on same FS.
    let status = Command::new("cp")
        .args(["--reflink=auto", "-f"])
        .arg(src)
        .arg(dst)
        .status()
        .map_err(|e| BootError::Stage(e.to_string()))?;
    if !status.success() {
        std::fs::copy(src, dst).map_err(|e| BootError::Stage(e.to_string()))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn create_tap(artifacts: &BootArtifacts, tap_name: &str) -> Result<(), BootError> {
    let script = format!(
        "/usr/sbin/ip link del {tap} 2>/dev/null || true; \
         /usr/sbin/ip tuntap add dev {tap} mode tap; \
         /usr/sbin/ip addr flush dev {tap} 2>/dev/null || true; \
         /usr/sbin/ip addr add 172.16.0.1/28 dev {tap}; \
         /usr/sbin/ip link set {tap} up",
        tap = tap_name
    );
    run_priv(artifacts, &["/bin/bash", "-c", &script]).map_err(BootError::Tap)
}

pub fn destroy_tap_name(tap_name: &str) {
    if let Ok(artifacts) = BootArtifacts::from_env() {
        destroy_tap(&artifacts, tap_name);
    } else {
        let _ = Command::new("ip")
            .args(["link", "del", tap_name])
            .status();
    }
}

#[cfg(target_os = "linux")]
fn destroy_tap(artifacts: &BootArtifacts, tap_name: &str) {
    let script = format!("/usr/sbin/ip link del {tap} 2>/dev/null || true", tap = tap_name);
    let _ = run_priv(artifacts, &["/bin/bash", "-c", &script]);
}

pub fn cleanup_jail(chroot_base: &Path, jail_id: &str) {
    let path = chroot_base.join("firecracker").join(jail_id);
    let _ = Command::new("rm").args(["-rf"]).arg(&path).status();
    // If permission denied inside root-owned bits, try priv helper.
    if path.exists() {
        if let Ok(artifacts) = BootArtifacts::from_env() {
            let script = format!("rm -rf '{}'", path.display());
            let _ = run_priv(&artifacts, &["/bin/bash", "-c", &script]);
        }
    }
}

#[cfg(target_os = "linux")]
fn run_priv(artifacts: &BootArtifacts, args: &[&str]) -> Result<(), String> {
    if let Some(priv_bin) = &artifacts.priv_bin {
        let status = Command::new(priv_bin)
            .args(args)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            return Ok(());
        }
        return Err(format!("priv helper failed: {status}"));
    }
    // Fallback: try direct (works if already root).
    let status = Command::new(args[0])
        .args(&args[1..])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed: {status}"))
    }
}

#[cfg(target_os = "linux")]
fn configure_vm(api_sock: &Path, calls: &[ApiCall]) -> Result<(), BootError> {
    use std::os::unix::net::UnixStream;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        match UnixStream::connect(api_sock) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .map_err(BootError::Spawn)?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .map_err(BootError::Spawn)?;
                for call in calls {
                    let body = serde_json::to_string(&call.body).expect("body");
                    if let Err(err) = put_json(&mut stream, call.path, &body) {
                        // reconnect for next attempts if keep-alive broke
                        last = err.clone();
                        if let Ok(mut s2) = UnixStream::connect(api_sock) {
                            s2.set_read_timeout(Some(Duration::from_secs(5)))
                                .map_err(BootError::Spawn)?;
                            s2.set_write_timeout(Some(Duration::from_secs(5)))
                                .map_err(BootError::Spawn)?;
                            put_json(&mut s2, call.path, &body).map_err(|e| BootError::Api {
                                path: call.path,
                                detail: e,
                            })?;
                            stream = s2;
                            continue;
                        }
                        return Err(BootError::Api {
                            path: call.path,
                            detail: err,
                        });
                    }
                }
                return Ok(());
            }
            Err(err) => last = err.to_string(),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(BootError::Api {
        path: "/",
        detail: last,
    })
}

#[cfg(target_os = "linux")]
fn put_json(stream: &mut std::os::unix::net::UnixStream, path: &str, body: &str) -> Result<(), String> {
    let req = format!(
        "PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    // Read until we have status line + Content-Length body or timeout.
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(hdr_end) = find_header_end(&buf) {
                    if response_complete(&buf, hdr_end) {
                        break;
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock || err.kind() == std::io::ErrorKind::TimedOut => {
                if !buf.is_empty() {
                    break;
                }
                return Err(err.to_string());
            }
            Err(err) => return Err(err.to_string()),
        }
        if buf.len() > 64 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
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

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn response_complete(buf: &[u8], hdr_end: usize) -> bool {
    let headers = String::from_utf8_lossy(&buf[..hdr_end]);
    if let Some(cl) = headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        return buf.len() >= hdr_end + cl;
    }
    // No body (204)
    true
}

pub fn teardown_vm(vm: &mut BootedVm) {
    let _ = vm.child.kill();
    let _ = vm.child.wait();
    destroy_tap_name(&vm.tap_name);
    cleanup_jail(&vm.chroot_base, &vm.jail_id);
}
