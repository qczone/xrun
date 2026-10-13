//! A deployment belongs to its first proven manager, never to an anonymous probe.
use super::*;
use std::{path::PathBuf, sync::Mutex};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerState {
    network_id: String,
    manager_id: String,
    #[serde(default)]
    authority: Option<crate::membership::AuthorityState>,
    #[serde(default)]
    acknowledgements: std::collections::HashMap<String, (u64, u64, String)>,
}
pub(super) struct Admission {
    path: PathBuf,
    owner: Mutex<Option<OwnerState>>,
}
impl Admission {
    pub(super) fn open(dir: &std::path::Path) -> Result<Self> {
        let path = dir.join("owner.json");
        let owner = match std::fs::read(&path) {
            Ok(bytes) => {
                let owner: OwnerState = serde_json::from_slice(&bytes)
                    .context(ErrorCode::InvalidRelay.error("invalid persisted relay owner"))?;
                RelayOwner {
                    network_id: owner.network_id.clone(),
                    manager_id: owner.manager_id.clone(),
                }
                .validate()?;
                if let Some(state) = &owner.authority {
                    state.verify(&owner.network_id)?;
                    if state.roster.roster.manager_id != owner.manager_id {
                        bail!(ErrorCode::InvalidRelay.error("owner and authority differ"));
                    }
                }
                Some(owner)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path,
            owner: Mutex::new(owner),
        })
    }
    pub(super) fn owner(&self) -> Option<RelayOwner> {
        self.owner.lock().unwrap().as_ref().map(|owner| RelayOwner {
            network_id: owner.network_id.clone(),
            manager_id: owner.manager_id.clone(),
        })
    }
    pub(super) fn authority(&self) -> Option<crate::membership::AuthorityState> {
        self.owner
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|owner| owner.authority.clone())
    }
    pub(super) fn check(&self, network: &str, claiming: bool) -> Result<()> {
        match self.owner() {
            Some(owner) if owner.network_id != network => {
                bail!(ErrorCode::RelayClaimed.error("relay already belongs to another network"))
            }
            None if !claiming => {
                bail!(ErrorCode::RelayUnclaimed.error("create a network before using this relay"))
            }
            _ => Ok(()),
        }
    }
    pub(super) fn publish(&self, state: &crate::membership::AuthorityState) -> Result<bool> {
        state.verify(&state.roster.roster.network_id)?;
        state.require_fresh(now_ms())?;
        let mut saved = self.owner.lock().unwrap();
        if let Some(previous) = saved.as_ref() {
            if previous.network_id != state.roster.roster.network_id {
                bail!(ErrorCode::RelayClaimed.error("cannot replace another network's owner"));
            }
            if let Some(previous) = &previous.authority {
                previous.roster.check_successor(&state.roster)?;
                if previous.roster.roster.version == state.roster.roster.version
                    && previous.lease.lease.issued_at_ms >= state.lease.lease.issued_at_ms
                {
                    return Ok(false);
                }
            } else if previous.manager_id != state.roster.roster.manager_id {
                bail!(
                    ErrorCode::RelayClaimed
                        .error("the existing owner must publish its signed authority first")
                );
            }
        }
        let next = OwnerState {
            network_id: state.roster.roster.network_id.clone(),
            manager_id: state.roster.roster.manager_id.clone(),
            authority: Some(state.clone()),
            acknowledgements: saved
                .as_ref()
                .map(|owner| owner.acknowledgements.clone())
                .unwrap_or_default(),
        };
        crate::config::atomic_private_write(&self.path, &serde_json::to_vec(&next)?)?;
        *saved = Some(next);
        Ok(true)
    }
    pub(super) fn authenticate(&self, proof: &Proof, manager: bool) -> Result<bool> {
        if self.owner().is_none() && !manager {
            bail!(ErrorCode::RelayUnclaimed.error("create a network before using this relay"));
        }
        let changed = if let Some(authority) = &proof.authority {
            self.publish(authority)?
        } else {
            false
        };
        let state = self.authority().context(
            ErrorCode::RosterExpired.error("manager has not published membership freshness"),
        )?;
        state.require_fresh(now_ms())?;
        if proof.authority_generation != state.roster.roster.authority_generation {
            bail!(
                ErrorCode::MembershipChanged.error("renew the connection after management changed")
            );
        }
        state.roster.peer(
            &crate::crypto::cert_der(&proof.cert_pem)?,
            Some(&proof.device_id),
        )?;
        Ok(changed)
    }
    pub(super) fn acknowledge(
        &self,
        device: &str,
        generation: u64,
        version: u64,
        hash: &str,
    ) -> Result<()> {
        let mut saved = self.owner.lock().unwrap();
        let owner = saved
            .as_mut()
            .context(ErrorCode::InvalidAck.error("authority missing"))?;
        let state = owner
            .authority
            .as_ref()
            .context(ErrorCode::InvalidAck.error("authority missing"))?;
        let roster = &state.roster.roster;
        if generation < roster.authority_generation
            || (generation == roster.authority_generation && version < roster.version)
        {
            return Ok(());
        }
        if generation != roster.authority_generation
            || version != roster.version
            || hash != state.roster.hash()?
        {
            bail!(ErrorCode::InvalidAck.error("authority acknowledgement differs"));
        }
        let previous = owner
            .acknowledgements
            .insert(device.into(), (generation, version, hash.into()));
        if let Err(error) =
            crate::config::atomic_private_write(&self.path, &serde_json::to_vec(owner)?)
        {
            if let Some(previous) = previous {
                owner.acknowledgements.insert(device.into(), previous);
            } else {
                owner.acknowledgements.remove(device);
            }
            return Err(error);
        }
        Ok(())
    }
    pub(super) fn pending(&self) -> Result<Vec<String>> {
        let saved = self.owner.lock().unwrap();
        let Some(owner) = saved.as_ref() else {
            return Ok(Vec::new());
        };
        let Some(state) = owner.authority.as_ref() else {
            return Ok(Vec::new());
        };
        let binding = (
            state.roster.roster.authority_generation,
            state.roster.roster.version,
            state.roster.hash()?,
        );
        Ok(state
            .roster
            .roster
            .members
            .iter()
            .filter(|member| {
                !member.revoked && owner.acknowledgements.get(&member.device_id) != Some(&binding)
            })
            .map(|member| member.device_id.clone())
            .collect())
    }
    pub(super) fn claim(&self, network: &str, device: &str, manager: bool) -> Result<RelayOwner> {
        if !manager {
            bail!(ErrorCode::NotManager.error("only a proven manager can claim a relay"));
        }
        let owner = self.owner().context(
            ErrorCode::RosterExpired
                .error("publish authority with the manager proof before claiming"),
        )?;
        if owner.network_id != network || owner.manager_id != device {
            bail!(ErrorCode::RelayClaimed.error("relay belongs to another manager"));
        }
        Ok(owner)
    }
    pub(super) fn manager(&self, device: &str, proof: bool) -> Result<bool> {
        if proof && self.owner().is_none_or(|owner| owner.manager_id != device) {
            bail!(ErrorCode::NotManager.error("manager proof differs from current owner"));
        }
        Ok(proof)
    }
}

pub(super) async fn claim_route(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let selected = protocol(&headers)?;
    ProtocolRange::require(selected)?;
    app.admission.check(&network, true)?;
    Ok(negotiated(
        ws.max_message_size(MAX_MESSAGE)
            .max_frame_size(MAX_MESSAGE)
            .on_upgrade(move |mut ws| async move {
                let result = async {
                    let path = format!("/networks/{network}/claim");
                    let (device, manager) = member(&app, &mut ws, &network, &path, &permit).await?;
                    let owner = app.admission.claim(&network, &device, manager)?;
                    send(&mut ws, &RelayMessage::RelayInfo { owner: Some(owner) }).await
                }
                .await;
                finish(&mut ws, result).await;
            }),
        selected,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::{DEFAULT_LEASE_MS, Manager};
    use std::sync::{Arc, Barrier};
    fn authority(dir: &std::path::Path, name: &str) -> Result<crate::membership::AuthorityState> {
        Manager::create(
            dir,
            name,
            vec!["https://relay.example.com".into()],
            String::new(),
        )?
        .0
        .authority_state(DEFAULT_LEASE_MS)
    }
    #[test]
    fn first_claim_is_atomic_persistent_and_never_replaced() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let a = authority(&dir.path().join("a"), "manager-a")?;
        let b = authority(&dir.path().join("b"), "manager-b")?;
        let admission = Arc::new(Admission::open(dir.path())?);
        let barrier = Arc::new(Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let claims: Vec<_> = [a, b]
                .into_iter()
                .map(|state| {
                    let admission = admission.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        admission.publish(&state)
                    })
                })
                .collect();
            claims
                .into_iter()
                .map(|claim| claim.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let owner = admission.owner().unwrap();
        let state = admission.authority().unwrap();
        assert_eq!(admission.pending()?, vec![owner.manager_id.clone()]);
        admission.acknowledge(
            &owner.manager_id,
            state.roster.roster.authority_generation,
            state.roster.roster.version,
            &state.roster.hash()?,
        )?;
        let restored = Admission::open(dir.path())?;
        assert_eq!(restored.owner(), Some(owner.clone()));
        assert!(restored.pending()?.is_empty());
        assert_eq!(
            restored.claim(&owner.network_id, &owner.manager_id, true)?,
            owner
        );
        assert!(
            restored
                .claim(&owner.network_id, &"c".repeat(32), true)
                .is_err()
        );
        assert!(
            restored
                .check(&format!("net_{}", "c".repeat(52)), false)
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn incomplete_proof_and_failed_persistence_leave_admission_closed() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let state = authority(&dir.path().join("manager"), "manager-a")?;
        let admission = Admission::open(dir.path())?;
        let network = &state.roster.roster.network_id;
        let manager = &state.roster.roster.manager_id;
        assert!(admission.claim(network, manager, false).is_err());
        assert!(admission.claim(network, manager, true).is_err());
        assert!(admission.owner().is_none());
        std::fs::create_dir(dir.path().join("owner.json"))?;
        assert!(admission.publish(&state).is_err());
        assert!(admission.owner().is_none());
        std::fs::remove_dir(dir.path().join("owner.json"))?;
        admission.publish(&state)?;
        admission.claim(network, manager, true)?;
        std::fs::write(dir.path().join("owner.json"), b"broken")?;
        assert!(Admission::open(dir.path()).is_err());
        Ok(())
    }
}
