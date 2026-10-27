use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct TemplateArtifacts {
    pub rootfs: PathBuf,
    pub kernel: Option<PathBuf>,
    pub snap_vmstate: Option<PathBuf>,
    pub snap_mem: Option<PathBuf>,
}

pub fn resolve(template_id: &str) -> TemplateArtifacts {
    let base = std::env::var_os("SANDBOX_TEMPLATE_DIR").map(PathBuf::from);
    if let Some(dir) = base {
        let tdir = dir.join(template_id);
        let rootfs = tdir.join("rootfs.ext4");
        if rootfs.exists() {
            return TemplateArtifacts {
                rootfs,
                kernel: some_if_exists(tdir.join("vmlinux")),
                snap_vmstate: some_if_exists(tdir.join("vmstate")),
                snap_mem: some_if_exists(tdir.join("mem")),
            };
        }
    }
    TemplateArtifacts {
        rootfs: PathBuf::from(std::env::var_os("SANDBOX_ROOTFS").unwrap_or_default()),
        kernel: std::env::var_os("SANDBOX_KERNEL").map(PathBuf::from),
        snap_vmstate: None,
        snap_mem: None,
    }
}

fn some_if_exists(p: PathBuf) -> Option<PathBuf> {
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

pub fn ficlone_rootfs(src: &Path, dst: &Path) -> Result<(), String> {
    if dst.exists() {
        let _ = std::fs::remove_file(dst);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    for mode in ["always", "auto"] {
        let status = Command::new("cp")
            .args([format!("--reflink={mode}"), "-f".into()])
            .arg(src)
            .arg(dst)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            return Ok(());
        }
    }
    std::fs::copy(src, dst).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn has_snapshot(t: &TemplateArtifacts) -> bool {
    t.snap_vmstate.is_some() && t.snap_mem.is_some()
}
