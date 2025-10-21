use std::net::Ipv4Addr;

use anyhow::Result;
use gdbmi::raw::Value;

pub trait FrameworkCommandAdapter: Send + Sync {
    fn get_bt_command_name(&self) -> String;
    fn extract_id_from_metadata(&self, meta: &Value) -> Result<String>;
}

#[derive(Clone)]
pub struct GrpcAdapter;

impl FrameworkCommandAdapter for GrpcAdapter {
    fn get_bt_command_name(&self) -> String {
        "-get-remote-bt".to_string()
    }

    fn extract_id_from_metadata(&self, meta: &Value) -> Result<String> {
        let pid = meta.get_dict_entry("pid")?.expect_string_repr::<u64>()?;
        let ip_int = meta.get_dict_entry("ip")?.expect_string_repr::<u32>()?;
        let ip_str = Ipv4Addr::from(ip_int).to_string();
        Ok(format!("{}:-{}", ip_str, pid))
    }
}

// for now, seems like Nu shares pretty much same implementation with Grpc
#[derive(Clone)]
pub struct NuAdapter;

impl FrameworkCommandAdapter for NuAdapter {
    fn get_bt_command_name(&self) -> String {
        "-get-remote-bt".to_string()
    }

    fn extract_id_from_metadata(&self, meta: &Value) -> Result<String> {
        let pid = meta.get_dict_entry("pid")?.expect_string_repr::<u64>()?;
        let ip_int = meta.get_dict_entry("ip")?.expect_string_repr::<u32>()?;
        let ip_str = Ipv4Addr::from(ip_int).to_string();
        Ok(format!("{}:-{}", ip_str, pid))
    }
}

#[derive(Clone)]
pub struct ServiceWeaverAdapter;

impl FrameworkCommandAdapter for ServiceWeaverAdapter {
    fn get_bt_command_name(&self) -> String {
        "-serviceweaver-bt-remote".to_string()
    }

    fn extract_id_from_metadata(&self, meta: &Value) -> Result<String> {
        let ip_int = meta.get_dict_entry("ip")?.expect_string_repr::<u32>()?;
        let ip_str = Ipv4Addr::from(ip_int).to_string();
        Ok(format!("{}", ip_str))
    }
}
