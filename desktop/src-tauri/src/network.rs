use anyhow::Result;
use serde::{Deserialize, Serialize};
use xrun::config::Identity;

#[derive(Serialize)]
pub struct NetworkStatus {
    pub network_id: String,
    pub manager_id: String,
    pub manager_name: String,
    pub is_manager: bool,
    pub relay_addresses: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Invitation {
    pub link: String,
    pub allow: bool,
    pub expires_in: u64,
}

pub fn local_status() -> Result<NetworkStatus> {
    let id = Identity::load()?;
    let roster = xrun::network::current(&id)?;
    Ok(NetworkStatus {
        network_id: roster.roster.network_id.clone(),
        manager_id: roster.roster.manager_id.clone(),
        manager_name: roster.member(&roster.roster.manager_id)?.name.clone(),
        is_manager: id.device_id == roster.roster.manager_id,
        relay_addresses: roster.roster.relay_addresses,
    })
}
