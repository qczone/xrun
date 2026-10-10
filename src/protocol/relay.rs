//! Signed relay challenges and routing messages shared by endpoints and relays.
use super::{Data, TrafficQuery, TrafficReport};
use crate::{
    config::Identity,
    crypto,
    error::ErrorCode,
    membership::{self, Manager},
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Member certificate and challenge signatures binding one relay connection.
pub struct Proof {
    /// Immutable device ID.
    pub device_id: String,
    /// PEM member certificate binding the device key.
    pub cert_pem: String,
    /// PEM network root used to verify member identity.
    pub root_pem: String,
    /// Base64 P-256 signature over the domain-separated challenge binding.
    pub signature: String,
    /// Root-key signature for manager control registration and network traffic queries.
    pub manager_signature: Option<String>,
}
#[derive(Debug, Serialize)]
/// Fields signed together to bind a proof to its device, network, route and nonce.
pub struct ChallengeBinding<'a> {
    /// Root-key-derived network identifier.
    pub network: &'a str,
    /// Device identity included in the signed binding.
    pub device: &'a str,
    /// Operation path, resolved relative to cwd when permitted.
    pub path: &'a str,
    /// Fresh random relay challenge; proofs cannot be replayed with another nonce.
    pub nonce: &'a str,
}
impl Proof {
    pub(crate) fn create(
        id: &Identity,
        network: &str,
        path: &str,
        nonce: &str,
        manager: Option<&Manager>,
    ) -> Result<Self> {
        let binding = ChallengeBinding {
            network,
            device: &id.device_id,
            path,
            nonce,
        };
        Ok(Self {
            device_id: id.device_id.clone(),
            cert_pem: id.cert_pem.clone(),
            root_pem: id.ca_pem.clone(),
            signature: membership::sign(&id.key_pem, "relay-proof", &binding)?,
            manager_signature: manager.map(|m| m.sign_relay(&binding)).transpose()?,
        })
    }
    /// The network ID is the root key fingerprint, so the relay verifies
    /// members without storing a roster. Revocation remains an endpoint check;
    /// a revoked device can only appear as itself until its certificate expires.
    /// Returns whether the proof also holds the network root key.
    pub fn verify(&self, network: &str, path: &str, nonce: &str) -> Result<bool> {
        if network != format!("net_{}", crypto::ca_spki_pin(&self.root_pem)?) {
            bail!(ErrorCode::Unauthenticated.error("proof belongs to another network"))
        }
        crypto::verify_member_certificate(&self.cert_pem, &self.root_pem, &self.device_id)
            .map_err(|e| {
                anyhow::anyhow!(
                    ErrorCode::Unauthenticated.error(format!("invalid member certificate: {e}"))
                )
            })?;
        let binding = ChallengeBinding {
            network,
            device: &self.device_id,
            path,
            nonce,
        };
        membership::verify(&self.cert_pem, "relay-proof", &binding, &self.signature).map_err(
            |_| anyhow::anyhow!(ErrorCode::Unauthenticated.error("invalid member proof")),
        )?;
        let Some(signature) = &self.manager_signature else {
            return Ok(false);
        };
        membership::verify(&self.root_pem, "relay-manager", &binding, signature).map_err(|_| {
            anyhow::anyhow!(ErrorCode::Unauthenticated.error("invalid manager proof"))
        })?;
        Ok(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
/// Relay routing and handshake messages, outside the encrypted peer payload.
pub enum RelayMessage {
    /// Protocol 2: declare a completed source tunnel reclaimable, or reactivate
    /// it before any encrypted probe/request. The relay echoes acceptance.
    CacheState {
        /// True only after the complete operation has been consumed.
        idle: bool,
    },
    /// Fresh challenge required before connection admission.
    Challenge {
        /// Fresh random relay challenge; proofs cannot be replayed with another nonce.
        nonce: String,
    },
    /// None only for pairing, which the relay routes to the manager alone.
    Authenticate {
        /// Member challenge proof; None only for anonymous manager pairing.
        proof: Option<Proof>,
    },
    /// Control generation accepted by the relay.
    HelloAck {
        /// Random identity of one accepted target control connection.
        generation: String,
    },
    /// Session awaiting attachment by the current target control generation.
    Incoming {
        /// Random session handoff identifier.
        session_id: String,
    },
    /// Target rejects a generation-bound incoming session.
    Reject {
        /// Random session handoff identifier.
        session_id: String,
        /// Human-readable execution failure, when recorded.
        error: Box<Data>,
    },
    /// Both ciphertext legs have been joined.
    Connected {
        #[serde(default)]
        /// Whether ciphertext framing requires credit acknowledgments.
        flow_control: bool,
    },
    /// Relay-observed live control device IDs.
    Status {
        /// Device IDs with live relay control bindings.
        devices: Vec<String>,
    },
    /// Query observed ciphertext usage after authenticating the traffic connection.
    TrafficQuery {
        /// Period and device pagination; scope comes from the authenticated proof.
        query: TrafficQuery,
    },
    /// Traffic authentication accepted; the client may now submit its query.
    TrafficReady,
    /// Relay-owned traffic snapshot, never an endpoint delivery or Job receipt.
    Traffic {
        /// Authorized network or device traffic report.
        report: TrafficReport,
    },
    /// Machine code plus human-readable diagnostic; dispatch only on code.
    Error {
        /// Stable machine-readable error code.
        code: String,
        /// Human-readable detail; changes do not define machine semantics.
        message: String,
    },
}
impl RelayMessage {
    pub(crate) fn error(error: &anyhow::Error) -> Self {
        let Data::Error { code, message } = Data::error(error) else {
            unreachable!()
        };
        Self::Error { code, message }
    }
}

/// Validate the 26-character base32 private route shared by both relay implementations.
pub fn valid_relay_route(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'2'..=b'7'))
}
