//! Reuse rustls session caches without reusing configuration across different identities.
use super::*;
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};
type TlsCache<T> = OnceLock<Mutex<VecDeque<(String, Arc<T>)>>>;
fn cache<T>(
    store: &TlsCache<T>,
    key: String,
    build: impl FnOnce() -> Result<Arc<T>>,
) -> Result<Arc<T>> {
    let store = store.get_or_init(Mutex::default);
    if let Some((_, config)) = store
        .lock()
        .unwrap()
        .iter()
        .find(|(candidate, _)| *candidate == key)
    {
        return Ok(config.clone());
    }
    let config = build()?;
    let mut entries = store.lock().unwrap();
    if let Some((_, existing)) = entries.iter().find(|(candidate, _)| *candidate == key) {
        return Ok(existing.clone());
    }
    if entries.len() == 16 {
        entries.pop_front();
    }
    entries.push_back((key, config.clone()));
    Ok(config)
}
fn identity_key(id: &Identity) -> String {
    crate::protocol::sha256(format!("{}\0{}\0{}", id.ca_pem, id.cert_pem, id.key_pem).as_bytes())
}
pub(super) fn client_tls_config(id: &Identity) -> Result<Arc<rustls::ClientConfig>> {
    static CLIENT: TlsCache<rustls::ClientConfig> = OnceLock::new();
    cache(&CLIENT, identity_key(id), || build_client_tls_config(id))
}
pub(super) fn peer_server_config(id: &Identity) -> Result<Arc<rustls::ServerConfig>> {
    static SERVER: TlsCache<rustls::ServerConfig> = OnceLock::new();
    cache(&SERVER, identity_key(id), || build_peer_server_config(id))
}
pub(super) fn relay_tls_config(ca: &str) -> Result<Arc<rustls::ClientConfig>> {
    static RELAY: TlsCache<rustls::ClientConfig> = OnceLock::new();
    cache(&RELAY, crate::protocol::sha256(ca.as_bytes()), || {
        build_relay_tls_config(ca)
    })
}
