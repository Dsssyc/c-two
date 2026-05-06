//! Runtime identity helpers shared by SDK sessions.

use crate::RuntimeSessionError;

pub fn validate_server_id(server_id: &str) -> Result<(), RuntimeSessionError> {
    c2_config::validate_server_id(server_id).map_err(RuntimeSessionError::InvalidServerId)
}

pub fn ipc_address_for_server_id(server_id: &str) -> String {
    format!("ipc://{server_id}")
}

pub fn auto_server_id() -> String {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    format!("cc{:x}{uuid}", std::process::id())
}
