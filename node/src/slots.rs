use std::collections::HashMap;
use std::net::Ipv4Addr;

/// `172.16.0.0/28` has 16 addresses. `.1` is the host. Guests are `.2` through `.15`.
pub const PREFIX_LEN: u8 = 28;
pub const ADDRESS_COUNT: usize = 16;
pub const HOST_IP: Ipv4Addr = Ipv4Addr::new(172, 16, 0, 1);
pub const GUEST_FIRST: u8 = 2;
pub const GUEST_LAST: u8 = 15;
pub const GUEST_SLOTS: usize = (GUEST_LAST - GUEST_FIRST + 1) as usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub index: u8,
    pub ip: Ipv4Addr,
    pub mac: String,
    pub tap: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocated {
    pub sandbox_id: String,
    pub slot: Slot,
}

#[derive(Debug)]
pub struct SlotPool {
    free: Vec<Slot>,
    used: HashMap<String, Slot>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SlotError {
    Exhausted,
    Unknown,
}

impl SlotPool {
    pub fn new() -> Self {
        let mut free = Vec::with_capacity(GUEST_SLOTS);
        for octet in (GUEST_FIRST..=GUEST_LAST).rev() {
            free.push(slot_for(octet));
        }
        Self {
            free,
            used: HashMap::new(),
        }
    }

    pub fn free_count(&self) -> usize {
        self.free.len()
    }

    pub fn allocate(&mut self, sandbox_id: &str) -> Result<Slot, SlotError> {
        if self.used.contains_key(sandbox_id) {
            return Err(SlotError::Exhausted);
        }
        let slot = self.free.pop().ok_or(SlotError::Exhausted)?;
        self.used.insert(sandbox_id.to_string(), slot.clone());
        Ok(slot)
    }

    /// Releases the slot held by `sandbox_id`. Unknown ids are not a no-op success.
    pub fn release(&mut self, sandbox_id: &str) -> Result<Slot, SlotError> {
        let slot = self.used.remove(sandbox_id).ok_or(SlotError::Unknown)?;
        self.free.push(slot.clone());
        Ok(slot)
    }

    pub fn get(&self, sandbox_id: &str) -> Option<&Slot> {
        self.used.get(sandbox_id)
    }
}

impl Default for SlotPool {
    fn default() -> Self {
        Self::new()
    }
}

pub fn prefix_addresses() -> [Ipv4Addr; ADDRESS_COUNT] {
    let mut out = [Ipv4Addr::UNSPECIFIED; ADDRESS_COUNT];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = Ipv4Addr::new(172, 16, 0, i as u8);
    }
    out
}

fn slot_for(octet: u8) -> Slot {
    let index = octet - GUEST_FIRST;
    Slot {
        index,
        ip: Ipv4Addr::new(172, 16, 0, octet),
        mac: format!("06:00:AC:10:00:{octet:02X}"),
        tap: format!("tap{index}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn prefix_has_sixteen_addresses_and_host_is_dot_one() {
        let addrs = prefix_addresses();
        assert_eq!(addrs.len(), 16);
        assert_eq!(addrs[0], Ipv4Addr::new(172, 16, 0, 0));
        assert_eq!(addrs[1], HOST_IP);
        assert_eq!(addrs[15], Ipv4Addr::new(172, 16, 0, 15));
        assert!(addrs.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn guests_start_at_dot_two_and_are_not_reused_until_release() {
        let mut pool = SlotPool::new();
        assert_eq!(pool.free_count(), GUEST_SLOTS);
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for i in 0..GUEST_SLOTS {
            let id = format!("sbx_{i}");
            let slot = pool.allocate(&id).unwrap();
            assert!(seen.insert(slot.ip));
            assert_ne!(slot.ip, HOST_IP);
            assert!(slot.ip.octets()[3] >= GUEST_FIRST);
            ids.push((id, slot.ip));
        }
        assert_eq!(ids[0].1, Ipv4Addr::new(172, 16, 0, 2));
        assert_eq!(pool.allocate("sbx_overflow"), Err(SlotError::Exhausted));
        let (id, ip) = ids[3].clone();
        pool.release(&id).unwrap();
        assert_eq!(pool.release(&id), Err(SlotError::Unknown));
        let again = pool.allocate("sbx_reused").unwrap();
        assert_eq!(again.ip, ip);
        assert_eq!(pool.allocate("sbx_another"), Err(SlotError::Exhausted));
    }
}
