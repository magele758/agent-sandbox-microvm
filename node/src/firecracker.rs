use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiCall {
    pub method: &'static str,
    pub path: &'static str,
    pub body: Value,
}

#[derive(Debug, Clone)]
pub struct BootParams<'a> {
    pub vcpu: u32,
    pub mem_mib: u32,
    pub rootfs_path: &'a str,
    pub tap_name: &'a str,
    pub guest_mac: &'a str,
    pub kernel_path: &'a str,
}

#[derive(Serialize)]
struct MachineConfig {
    vcpu_count: u32,
    mem_size_mib: u32,
    smt: bool,
    track_dirty_pages: bool,
}

#[derive(Serialize)]
struct Drive<'a> {
    drive_id: &'a str,
    path_on_host: &'a str,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Serialize)]
struct NetIface<'a> {
    iface_id: &'a str,
    guest_mac: &'a str,
    host_dev_name: &'a str,
}

#[derive(Serialize)]
struct BootSource<'a> {
    kernel_image_path: &'a str,
    boot_args: &'a str,
}

#[derive(Serialize)]
struct InstanceAction<'a> {
    action_type: &'a str,
}

#[derive(Serialize)]
struct MemBackend<'a> {
    backend_path: &'a str,
    backend_type: &'a str,
}

#[derive(Serialize)]
struct SnapshotLoad<'a> {
    snapshot_path: &'a str,
    mem_backend: MemBackend<'a>,
}

/// Ordered unix-socket calls for a cold boot. Does not fork Firecracker.
pub fn boot_calls(params: &BootParams<'_>) -> Vec<ApiCall> {
    let machine = MachineConfig {
        vcpu_count: params.vcpu,
        mem_size_mib: params.mem_mib,
        smt: false,
        track_dirty_pages: false,
    };
    let drive = Drive {
        drive_id: "rootfs",
        path_on_host: params.rootfs_path,
        is_root_device: true,
        is_read_only: false,
    };
    let net = NetIface {
        iface_id: "eth0",
        guest_mac: params.guest_mac,
        host_dev_name: params.tap_name,
    };
    let boot = BootSource {
        kernel_image_path: params.kernel_path,
        boot_args: "console=ttyS0 reboot=k panic=1 pci=off init=/sbin/init",
    };
    let start = InstanceAction {
        action_type: "InstanceStart",
    };
    vec![
        ApiCall {
            method: "PUT",
            path: "/machine-config",
            body: serde_json::to_value(&machine).expect("machine-config"),
        },
        ApiCall {
            method: "PUT",
            path: "/drives/rootfs",
            body: serde_json::to_value(&drive).expect("drive"),
        },
        ApiCall {
            method: "PUT",
            path: "/network-interfaces/eth0",
            body: serde_json::to_value(&net).expect("net"),
        },
        ApiCall {
            method: "PUT",
            path: "/boot-source",
            body: serde_json::to_value(&boot).expect("boot"),
        },
        ApiCall {
            method: "PUT",
            path: "/actions",
            body: serde_json::to_value(&start).expect("actions"),
        },
    ]
}

/// Snapshot load is not part of create in this slice.
pub fn snapshot_load_call(snapshot_path: &str, mem_backend_path: &str) -> ApiCall {
    let body = SnapshotLoad {
        snapshot_path,
        mem_backend: MemBackend {
            backend_path: mem_backend_path,
            backend_type: "File",
        },
    };
    ApiCall {
        method: "PUT",
        path: "/snapshot/load",
        body: serde_json::to_value(&body).expect("snapshot"),
    }
}

/// `jailer --id <sandboxID> --exec-file <firecracker> --uid 123 --gid 123 --cgroup-version 2`
pub fn jailer_argv(sandbox_id: &str, firecracker: &str) -> Vec<String> {
    vec![
        "jailer".to_string(),
        "--id".to_string(),
        sandbox_id.to_string(),
        "--exec-file".to_string(),
        firecracker.to_string(),
        "--uid".to_string(),
        "123".to_string(),
        "--gid".to_string(),
        "123".to_string(),
        "--cgroup-version".to_string(),
        "2".to_string(),
    ]
}

pub fn body_json(call: &ApiCall) -> String {
    serde_json::to_string(&call.body).expect("json")
}

/// Used by tests to show the action body is exactly the upstream object.
pub fn instance_start_body() -> Value {
    json!({"action_type": "InstanceStart"})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<ApiCall> {
        boot_calls(&BootParams {
            vcpu: 2,
            mem_mib: 2048,
            rootfs_path: "/images/rootfs.ext4",
            tap_name: "tap0",
            guest_mac: "06:00:AC:10:00:02",
            kernel_path: "/images/vmlinux",
        })
    }

    #[test]
    fn boot_calls_are_ordered_and_match_upstream_json() {
        let calls = sample();
        let paths: Vec<_> = calls.iter().map(|c| c.path).collect();
        assert_eq!(
            paths,
            vec![
                "/machine-config",
                "/drives/rootfs",
                "/network-interfaces/eth0",
                "/boot-source",
                "/actions",
            ]
        );
        assert!(calls.iter().all(|c| c.method == "PUT"));
        assert_eq!(
            body_json(&calls[0]),
            r#"{"vcpu_count":2,"mem_size_mib":2048,"smt":false,"track_dirty_pages":false}"#
        );
        assert_eq!(
            body_json(&calls[1]),
            r#"{"drive_id":"rootfs","path_on_host":"/images/rootfs.ext4","is_root_device":true,"is_read_only":false}"#
        );
        assert_eq!(
            body_json(&calls[2]),
            r#"{"iface_id":"eth0","guest_mac":"06:00:AC:10:00:02","host_dev_name":"tap0"}"#
        );
        assert_eq!(
            body_json(&calls[3]),
            r#"{"kernel_image_path":"/images/vmlinux","boot_args":"console=ttyS0 reboot=k panic=1 pci=off init=/sbin/init"}"#
        );
        assert!(body_json(&calls[3]).contains("init=/sbin/init"));
        assert_eq!(body_json(&calls[4]), r#"{"action_type":"InstanceStart"}"#);
        assert_eq!(calls.last().unwrap().path, "/actions");
    }

    #[test]
    fn snapshot_load_is_separate_from_create() {
        let call = snapshot_load_call("/images/snap", "/images/mem");
        assert_eq!(call.method, "PUT");
        assert_eq!(call.path, "/snapshot/load");
        assert_eq!(
            body_json(&call),
            r#"{"snapshot_path":"/images/snap","mem_backend":{"backend_path":"/images/mem","backend_type":"File"}}"#
        );
        assert!(sample().iter().all(|c| c.path != "/snapshot/load"));
    }

    #[test]
    fn jailer_argv_matches_spec() {
        let argv = jailer_argv("sbx_abc", "/usr/bin/firecracker");
        assert_eq!(
            argv,
            vec![
                "jailer",
                "--id",
                "sbx_abc",
                "--exec-file",
                "/usr/bin/firecracker",
                "--uid",
                "123",
                "--gid",
                "123",
                "--cgroup-version",
                "2",
            ]
        );
    }
}
