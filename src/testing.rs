//! Direct access needed by integration tests; no features or alternate build matrix.
//! Application adapters must use client/runtime/protocol/error instead.
pub use crate::{error, protocol};

/// Construct malformed configuration and fault-injection fixtures in isolated homes.
pub mod config {
    pub use crate::config::{
        DaemonConfig, Identity, NetworkIdentity, ServerConfig, atomic_private_write, device_dir,
        home_dir, instance_running, read, update_permission, write,
    };
}
/// Probe and stop isolated daemon fixtures, including transport and lifecycle failures.
pub mod control {
    /// Request a real private-IPC reload of an isolated daemon's authorization.
    pub async fn refresh_access(dir: &std::path::Path) -> anyhow::Result<()> {
        crate::ipc::refresh_access(dir).await
    }
    pub use crate::control::{request_shutdown, state};
}
/// Construct pinned TLS identities and forged certificate/proof inputs.
pub mod crypto {
    pub use crate::crypto::{
        anonymous_tls_config, ca_spki_pin, cert_der, certificate_expiring, discover_ca,
        http_client, load_or_create_server, new_device_request, relay_tls_config,
        renew_device_request,
    };
}
/// Launch real daemon fixtures or hold their instance lock while injecting failures.
pub mod daemon {
    pub use crate::daemon::{instance_lock, run};
}
/// Exercise authority transactions, signature validation and monotonic cache rules.
pub mod membership {
    pub use crate::membership::{
        Manager, ReceiptAck, RosterCache, SignedRoster, device_name, sign,
    };
}
/// Raw transport for frame replay, ciphertext interception and forged acknowledgments.
pub mod net {
    pub use crate::net::{
        Io, Ws, close, receive, receive_bytes, send, send_bytes, send_file, websocket_at,
    };
}
/// Pair isolated peers and exchange signed updates without weakening protocol tests.
pub mod network {
    pub use crate::network::{PeerState, authenticate, invite, manager, observe};
}
/// Verify process group cleanup using an actual child fixture.
pub mod process {
    pub use crate::process::{force_kill, spawn, terminate};
}
/// Run private relay fixtures and discover their generated secret deployment route.
pub mod relay {
    pub use crate::relay::{addresses, deployment_link, run};
}
/// Check capture format and platform failures with the real capture implementation.
pub mod screenshot {
    pub use crate::screenshot::capture;
}
/// Exercise member, pairing and server TLS policies against adversarial peers.
pub mod secure {
    pub use crate::secure::{
        Purpose, client, exchange_client, exchange_server, pairing_client, server,
    };
}
/// Start HTTP admission fixtures for transport limits and malformed requests.
pub mod server {
    pub use crate::server::run;
}
/// Query and seed isolated task databases for quota, recovery and failure assertions.
pub mod store {
    pub use crate::store::TaskStore;
}
/// Check local file replacement and streaming upload with real filesystem paths.
pub mod transfer {
    pub use crate::transfer::{prepare_upload, save_local};
}

/// Build a relay challenge response using the fixture's private identity and root key.
pub fn relay_proof(
    id: &config::Identity,
    network: &str,
    path: &str,
    nonce: &str,
    manager: Option<&membership::Manager>,
) -> anyhow::Result<protocol::Proof> {
    protocol::Proof::create(id, network, path, nonce, manager)
}

/// Replace an isolated task snapshot to construct otherwise unreachable states.
/// Production lifecycle mutations must use TaskStore's constrained operations.
pub fn replace_task_fixture(store: &store::TaskStore, job: &protocol::Job) -> anyhow::Result<()> {
    store.replace_fixture(job)
}
