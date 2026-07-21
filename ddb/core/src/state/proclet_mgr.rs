use std::fmt::{Debug, Display};

use dashmap::DashMap;
use tracing::debug;

#[derive(Debug)]
pub struct ProcletMgr {
    caladan_ip_to_sid: DashMap<u32, u64>,
}

impl Display for ProcletMgr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ProcletMgr {{ caladan_ip_to_sid: {:?} }}",
            self.caladan_ip_to_sid
        )
    }
}

impl ProcletMgr {
    pub fn new() -> Self {
        Self {
            caladan_ip_to_sid: DashMap::new(),
        }
    }

    pub fn register_owner_session(&self, caladan_ip: u32, sid: u64) {
        self.caladan_ip_to_sid.insert(caladan_ip, sid);
        debug!("Registered caladan_ip: {} with sid: {}", caladan_ip, sid);
    }

    pub fn remove_owner_session(&self, sid: u64) {
        self.caladan_ip_to_sid
            .retain(|_, owner_sid| *owner_sid != sid);
    }

    pub fn session_id_for_caladan_ip(&self, caladan_ip: u32) -> Option<u64> {
        self.caladan_ip_to_sid
            .get(&caladan_ip)
            .map(|sid| sid.value().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proclet_manager_registers_and_resolves_caladan_ip() {
        let mgr = ProcletMgr::new();

        mgr.register_owner_session(7, 99);

        assert_eq!(mgr.session_id_for_caladan_ip(7), Some(99));
        assert_eq!(mgr.session_id_for_caladan_ip(8), None);

        mgr.remove_owner_session(99);
        assert_eq!(mgr.session_id_for_caladan_ip(7), None);
    }
}
