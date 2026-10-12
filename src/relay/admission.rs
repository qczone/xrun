//! A deployment belongs to its first proven manager, never to an anonymous probe.
use super::*;
use std::{path::PathBuf, sync::Mutex};

pub(super) struct Admission {
    path: PathBuf,
    owner: Mutex<Option<RelayOwner>>,
}
impl Admission {
    pub(super) fn open(dir: &std::path::Path) -> Result<Self> {
        let path = dir.join("owner.json");
        let owner = match std::fs::read(&path) {
            Ok(bytes) => {
                let owner: RelayOwner = serde_json::from_slice(&bytes)
                    .context(ErrorCode::InvalidRelay.error("invalid persisted relay owner"))?;
                owner.validate()?;
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
        self.owner.lock().unwrap().clone()
    }
    pub(super) fn check(&self, network: &str, claiming: bool) -> Result<()> {
        match self.owner() {
            Some(owner) if owner.network_id != network => bail!(ErrorCode::RelayClaimed.error(
                "relay already belongs to a network; ask its manager for a join invitation"
            )),
            None if !claiming => bail!(
                ErrorCode::RelayUnclaimed
                    .error("create a network with xrun up before using this relay")
            ),
            _ => Ok(()),
        }
    }
    pub(super) fn claim(&self, network: &str, device: &str, manager: bool) -> Result<RelayOwner> {
        if !manager {
            bail!(ErrorCode::NotManager.error("only a proven network manager can claim a relay"));
        }
        let next = RelayOwner {
            network_id: network.into(),
            manager_id: device.into(),
        };
        next.validate()?;
        let mut owner = self.owner.lock().unwrap();
        if let Some(current) = owner.as_ref() {
            if current != &next {
                bail!(ErrorCode::RelayClaimed.error("relay is already bound to another manager"));
            }
            return Ok(current.clone());
        }
        // Persist while holding the claim lock; failure must never open admission.
        crate::config::atomic_private_write(&self.path, &serde_json::to_vec(&next)?)?;
        *owner = Some(next.clone());
        Ok(next)
    }
    pub(super) fn manager(&self, device: &str, proof: bool) -> Result<bool> {
        if proof && self.owner().is_none_or(|owner| owner.manager_id != device) {
            bail!(ErrorCode::NotManager.error("manager proof differs from the bound relay owner"));
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
    ProtocolRange::require(selected, 3)?;
    app.admission.check(&network, true)?;
    Ok(negotiated(
        ws.max_message_size(MAX_MESSAGE)
            .max_frame_size(MAX_MESSAGE)
            .on_upgrade(move |mut ws| async move {
                let result = async {
                    let path = format!("/networks/{network}/claim");
                    let (device, manager) = member(&mut ws, &network, &path, &permit).await?;
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
    use std::sync::{Arc, Barrier};

    #[test]
    fn first_claim_is_atomic_persistent_and_never_replaced() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let admission = Arc::new(Admission::open(dir.path())?);
        let barrier = Arc::new(Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let claims: Vec<_> = ["a", "b"]
                .into_iter()
                .map(|letter| {
                    let admission = admission.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        admission.claim(
                            &format!("net_{}", letter.repeat(52)),
                            &format!("dev_{}", letter.repeat(32)),
                            true,
                        )
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
        let restored = Admission::open(dir.path())?;
        assert_eq!(restored.owner(), Some(owner.clone()));
        assert_eq!(
            restored.claim(&owner.network_id, &owner.manager_id, true)?,
            owner
        );
        let error = restored
            .claim(&owner.network_id, &format!("dev_{}", "c".repeat(32)), true)
            .unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::RelayClaimed));
        assert!(
            restored
                .check(&format!("net_{}", "c".repeat(52)), false)
                .is_err()
        );
        assert!(restored.check(&owner.network_id, false).is_ok());
        Ok(())
    }

    #[test]
    fn incomplete_proof_and_failed_persistence_leave_admission_closed() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let admission = Admission::open(dir.path())?;
        let network = format!("net_{}", "a".repeat(52));
        let manager = format!("dev_{}", "a".repeat(32));
        assert!(admission.claim(&network, &manager, false).is_err());
        assert!(admission.owner().is_none());
        assert!(admission.check(&network, false).is_err());
        std::fs::create_dir(dir.path().join("owner.json"))?;
        assert!(admission.claim(&network, &manager, true).is_err());
        assert!(admission.owner().is_none());
        std::fs::remove_dir(dir.path().join("owner.json"))?;
        admission.claim(&network, &manager, true)?;
        std::fs::write(dir.path().join("owner.json"), b"broken")?;
        assert!(Admission::open(dir.path()).is_err());
        Ok(())
    }
}
