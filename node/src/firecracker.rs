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
    pub guest_ip: &'a str,
    pub enable_net: bool,
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
    boot_args: String,
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

/// Jailer instance ids allow only alphanumeric chars.
pub fn jailer_id(sandbox_id: &str) -> String {
    sandbox_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { 'x' })
        .collect()
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
    let boot = BootSource {
        kernel_image_path: params.kernel_path,
        boot_args: format!(
            "console=ttyS0 reboot=k panic=1 pci=off init=/sbin/init ip={}::172.16.0.1:255.255.255.240:sbx:eth0:off",
            params.guest_ip
        ),
    };
    let start = InstanceAction {
        action_type: "InstanceStart",
    };
    let mut calls = vec![
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
    ];
    if params.enable_net {
        let net = NetIface {
            iface_id: "eth0",
            guest_mac: params.guest_mac,
            host_dev_name: params.tap_name,
        };
        calls.push(ApiCall {
            method: "PUT",
            path: "/network-interfaces/eth0",
            body: serde_json::to_value(&net).expect("net"),
        });
    }
    calls.push(ApiCall {
        method: "PUT",
        path: "/boot-source",
        body: serde_json::to_value(&boot).expect("boot"),
    });
    calls.push(ApiCall {
        method: "PUT",
        path: "/actions",
        body: serde_json::to_value(&start).expect("actions"),
    });
    calls
}

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

pub fn jailer_argv(
    jail_id: &str,
    firecracker: &str,
    chroot_base: &str,
    uid: u32,
    gid: u32,
) -> Vec<String> {
    vec![
        "jailer".to_string(),
        "--id".to_string(),
        jail_id.to_string(),
        "--exec-file".to_string(),
        firecracker.to_string(),
        "--uid".to_string(),
        uid.to_string(),
        "--gid".to_string(),
        gid.to_string(),
        "--cgroup-version".to_string(),
        "2".to_string(),
        "--chroot-base-dir".to_string(),
        chroot_base.to_string(),
    ]
}

pub fn body_json(call: &ApiCall) -> String {
    serde_json::to_string(&call.body).expect("json")
}

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
            rootfs_path: "/rootfs.ext4",
            tap_name: "tap0",
            guest_mac: "06:00:AC:10:00:02",
            kernel_path: "/vmlinux",
            guest_ip: "172.16.0.2",
            enable_net: true,
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
            r#"{"drive_id":"rootfs","path_on_host":"/rootfs.ext4","is_root_device":true,"is_read_only":false}"#
        );
        assert_eq!(
            body_json(&calls[2]),
            r#"{"iface_id":"eth0","guest_mac":"06:00:AC:10:00:02","host_dev_name":"tap0"}"#
        );
        let boot = body_json(&calls[3]);
        assert!(boot.contains("/vmlinux"));
        assert!(boot.contains("init=/sbin/init"));
        assert!(boot.contains("ip=172.16.0.2::172.16.0.1:255.255.255.240"));
        assert_eq!(body_json(&calls[4]), r#"{"action_type":"InstanceStart"}"#);
    }

    #[test]
    fn boot_calls_can_skip_net() {
        let calls = boot_calls(&BootParams {
            vcpu: 1,
            mem_mib: 512,
            rootfs_path: "/rootfs.ext4",
            tap_name: "tap0",
            guest_mac: "06:00:AC:10:00:02",
            kernel_path: "/vmlinux",
            guest_ip: "172.16.0.2",
            enable_net: false,
        });
        assert!(calls.iter().all(|c| c.path != "/network-interfaces/eth0"));
    }

    #[test]
    fn jailer_id_strips_underscore() {
        assert_eq!(jailer_id("sbx_abc"), "sbxxabc");
    }

    #[test]
    fn snapshot_load_is_separate_from_create() {
        let call = snapshot_load_call("/images/snap", "/images/mem");
        assert_eq!(call.method, "PUT");
        assert_eq!(call.path, "/snapshot/load");
        assert!(sample().iter().all(|c| c.path != "/snapshot/load"));
    }

    #[test]
    fn jailer_argv_matches_spec() {
        let argv = jailer_argv("sbxxabc", "/usr/bin/firecracker", "/tmp/sbxj", 1000, 1000);
        assert_eq!(
            argv,
            vec![
                "jailer",
                "--id",
                "sbxxabc",
                "--exec-file",
                "/usr/bin/firecracker",
                "--uid",
                "1000",
                "--gid",
                "1000",
                "--cgroup-version",
                "2",
                "--chroot-base-dir",
                "/tmp/sbxj",
            ]
        );
    }
}
