use dashmap::DashMap;
use tracing::debug;

use crate::discovery::discovery_message_producer::UserDataMap;

pub struct ProcletMgr {
    caladan_ip_to_sid: DashMap<u32, u64>,
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
