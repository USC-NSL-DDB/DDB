use std::fmt::{Debug, Display};

use dashmap::DashMap;
use tracing::debug;

use crate::discovery::discovery_message_producer::UserDataMap;

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

    pub fn register_caladan_ip(&self, caladan_ip: u32, sid: u64) {
        self.caladan_ip_to_sid.insert(caladan_ip, sid);
        debug!("Registered caladan_ip: {} with sid: {}", caladan_ip, sid);
    }

    pub fn get_sid(&self, caladan_ip: u32) -> Option<u64> {
        self.caladan_ip_to_sid
            .get(&caladan_ip)
            .map(|sid| sid.value().clone())
    }
}

pub fn get_caladan_ip_from_user_data(user_data: &UserDataMap) -> Option<u32> {
    user_data.as_ref().and_then(|data| {
        data.get("caladan_ip")
            .and_then(|ip_str| ip_str.parse::<u32>().ok())
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn proclet_manager_registers_and_resolves_caladan_ip() {
        let mgr = ProcletMgr::new();

        mgr.register_caladan_ip(7, 99);

        assert_eq!(mgr.get_sid(7), Some(99));
        assert_eq!(mgr.get_sid(8), None);
    }

    #[test]
    fn caladan_ip_is_extracted_only_from_valid_user_data() {
        let valid = Some(HashMap::from([(
            "caladan_ip".to_string(),
            "42".to_string(),
        )]));
        let invalid = Some(HashMap::from([(
            "caladan_ip".to_string(),
            "not-a-number".to_string(),
        )]));

        assert_eq!(get_caladan_ip_from_user_data(&valid), Some(42));
        assert_eq!(get_caladan_ip_from_user_data(&invalid), None);
        assert_eq!(get_caladan_ip_from_user_data(&None), None);
    }
}
