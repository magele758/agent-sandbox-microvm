use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;

#[derive(Debug, Clone)]
pub struct RunningSandbox {
    pub mem_mib: i64,
    pub guest_ip: Ipv4Addr,
    pub env_vars: BTreeMap<String, String>,
    pub allow_internet: bool,
}

#[derive(Debug, Default)]
pub struct SandboxTable {
    running: HashMap<String, RunningSandbox>,
}

impl SandboxTable {
    pub fn insert(&mut self, id: String, sandbox: RunningSandbox) {
        self.running.insert(id, sandbox);
    }

    pub fn remove(&mut self, id: &str) -> Option<RunningSandbox> {
        self.running.remove(id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.running.contains_key(id)
    }

    pub fn running_count(&self) -> i64 {
        self.running.len() as i64
    }

    /// Sum of configured mem of sandboxes in state running, not RSS.
    pub fn committed_mem_mib(&self) -> i64 {
        self.running.values().map(|s| s.mem_mib).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_mem_tracks_running_only() {
        let mut table = SandboxTable::default();
        assert_eq!(table.committed_mem_mib(), 0);
        table.insert(
            "a".into(),
            RunningSandbox {
                mem_mib: 2048,
                guest_ip: Ipv4Addr::new(172, 16, 0, 2),
                env_vars: BTreeMap::new(),
                allow_internet: true,
            },
        );
        table.insert(
            "b".into(),
            RunningSandbox {
                mem_mib: 512,
                guest_ip: Ipv4Addr::new(172, 16, 0, 3),
                env_vars: BTreeMap::new(),
                allow_internet: false,
            },
        );
        assert_eq!(table.running_count(), 2);
        assert_eq!(table.committed_mem_mib(), 2560);
        table.remove("a").unwrap();
        assert_eq!(table.committed_mem_mib(), 512);
        assert_eq!(table.running_count(), 1);
        assert!(table.remove("missing").is_none());
    }
}
