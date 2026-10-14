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
/// Serve the desktop App's bridge in this process, for App tests without a helper binary.
pub mod bridge {
    use std::io::{BufReader, PipeReader, PipeWriter};
    /// Start a bridge on its own runtime thread and return the App's ends of the
    /// channel. Dropping the writer closes the bridge, as the App's exit does.
    pub fn spawn() -> std::io::Result<(PipeWriter, BufReader<PipeReader>)> {
        let (input, requests) = std::io::pipe()?;
        let (responses, output) = std::io::pipe()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        std::thread::spawn(move || {
            if let Err(error) =
                runtime.block_on(crate::bridge::serve(BufReader::new(input), output))
            {
                eprintln!("desktop bridge fixture failed: {error:#}");
            }
        });
        Ok((requests, BufReader::new(responses)))
    }
}
/// Launch real daemon fixtures or hold their instance lock while injecting failures.
pub mod daemon {
    pub use crate::daemon::{instance_lock, run};
    /// Run one actual background maintenance pass in an isolated identity home.
    pub async fn maintain_membership() -> anyhow::Result<()> {
        crate::daemon::maintain_membership(&mut 0).await
    }
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
        Io, SocketIo, Ws, close, receive, receive_bytes, send, send_bytes, send_chunk, send_file,
        send_logs, websocket_at,
    };
    /// Send a synthetic immutable upload, consuming the target's durable cursor.
    pub async fn send_transfer_bytes(ws: &mut Ws, bytes: &[u8]) -> anyhow::Result<()> {
        let (temp, size, hash) = crate::transfer::snapshot_input(std::io::Cursor::new(bytes))?;
        let crate::protocol::Data::TransferOffer {
            offset,
            skipped,
            sha256,
            ..
        } = receive(ws).await?
        else {
            anyhow::bail!("transfer offer")
        };
        anyhow::ensure!(sha256 == hash);
        if !skipped {
            crate::net::send_remaining(ws, temp.as_file(), size, offset, |_| Ok(())).await?;
        }
        Ok(())
    }
}
/// Pair isolated peers and exchange signed updates without weakening protocol tests.
pub mod network {
    pub use crate::network::{PeerState, invite, manager, observe};
    /// Verify relay membership state without touching the test runner's real profile.
    pub async fn authenticate(
        ws: &mut crate::net::Ws,
        id: Option<&crate::config::Identity>,
        network: &str,
        path: &str,
        manager: Option<&crate::membership::Manager>,
    ) -> anyhow::Result<()> {
        let state = crate::network::authenticate_read_only(ws, id, network, path, manager).await?;
        if path.ends_with("/control")
            && let Some(state) = state
        {
            crate::net::send(
                ws,
                &crate::protocol::RelayMessage::AuthorityAck {
                    authority_generation: state.roster.roster.authority_generation,
                    version: state.roster.roster.version,
                    hash: state.roster.hash()?,
                },
            )
            .await?;
        }
        Ok(())
    }
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
    /// Adapt a one-operation fake peer to current multiplexed and direct purposes.
    /// The fixture deliberately closes its whole channel after that stream.
    pub async fn operation_socket(
        mut ws: crate::net::Ws,
        cache: &crate::membership::RosterCache,
        network: &str,
        purpose: crate::secure::Purpose,
        id: &crate::config::Identity,
    ) -> anyhow::Result<crate::net::Ws> {
        if matches!(purpose, crate::secure::Purpose::Execute) {
            return Ok(ws);
        }
        anyhow::ensure!(matches!(purpose, crate::secure::Purpose::Channel));
        crate::net::send(
            &mut ws,
            &crate::protocol::Data::ChannelReady {
                device_id: id.device_id.clone(),
                protocol: crate::protocol::PROTOCOL,
            },
        )
        .await?;
        let (channel, mut incoming) = crate::multiplex::start(ws, yamux::Mode::Server);
        let mut ws = incoming
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("fixture channel closed"))?;
        tokio::spawn(async move {
            while channel.active() > 0 && channel.alive() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            channel.close();
        });
        let crate::protocol::Data::SessionProbe { roster_version } =
            crate::net::receive(&mut ws).await?
        else {
            anyhow::bail!("fixture membership probe");
        };
        let current = cache.require_fresh(network, crate::protocol::now_ms())?;
        crate::net::send(
            &mut ws,
            &crate::protocol::Data::Membership {
                roster: (roster_version != current.roster.roster.version).then_some(current.roster),
                lease: current.lease,
            },
        )
        .await?;
        Ok(ws)
    }
}
/// Start HTTP admission fixtures for transport limits and malformed requests.
pub mod server {
    pub use crate::server::run;
}
/// Query and seed isolated job databases for quota, recovery and failure assertions.
pub mod store {
    pub use crate::store::JobStore;
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

/// Replace an isolated job snapshot to construct otherwise unreachable states.
/// Production lifecycle mutations must use JobStore's constrained operations.
pub fn replace_job_fixture(store: &store::JobStore, job: &protocol::Job) -> anyhow::Result<()> {
    store.replace_fixture(job)
}

/// Retain a file copy and attach its metadata to a job in an isolated fixture.
pub async fn retain_attachment_fixture(
    store: &store::JobStore,
    job_id: &str,
    name: &str,
    bytes: &[u8],
) -> anyhow::Result<String> {
    let metadata = store
        .attachments()
        .save(std::io::Cursor::new(bytes), name)?;
    let id = metadata.id.clone();
    store.attach(job_id, metadata).await?;
    Ok(id)
}
