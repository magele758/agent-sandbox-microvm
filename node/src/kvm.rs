use std::path::Path;

/// Device presence only. False on non-Linux and when `/dev/kvm` is missing.
pub fn kvm_device_present() -> bool {
    cfg!(target_os = "linux") && Path::new("/dev/kvm").exists()
}

/// Jailer and Firecracker may run only when all three gates are open.
pub fn execution_allowed() -> bool {
    kvm_device_present() && std::env::var("SANDBOX_ENABLE_KVM").ok().as_deref() == Some("1")
}
