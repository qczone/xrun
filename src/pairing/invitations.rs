//! One daemon owns short secrets, registration replies and concurrent PAKE leases.
use super::{Command, Reply, random_chars};
use crate::{client::Invitation, config::Identity, error::ErrorCode, network, protocol::*};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc, oneshot};

pub(crate) struct Entry {
    pub(crate) id: String,
    pub(crate) locator: String,
    pub(crate) secret: String,
    pub(crate) token: String,
    pub(crate) address: String,
    pub(crate) expires_at: i64,
    pub(crate) lease: Arc<Semaphore>,
}
#[derive(Clone)]
struct Active {
    address: String,
    device: String,
    protocol: u32,
}
pub(crate) struct Invitations {
    active: Mutex<Option<Active>>,
    entries: Mutex<HashMap<String, Arc<Entry>>>,
    pending: Mutex<HashMap<String, Reply>>,
    commands: mpsc::Sender<Command>,
    receiver: Mutex<Option<mpsc::Receiver<Command>>>,
}
impl Invitations {
    pub(crate) fn new() -> Self {
        let (commands, receiver) = mpsc::channel(32);
        Self {
            active: Mutex::new(None),
            entries: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            commands,
            receiver: Mutex::new(Some(receiver)),
        }
    }
    pub(crate) fn receiver(&self) -> mpsc::Receiver<Command> {
        self.receiver
            .lock()
            .unwrap()
            .take()
            .expect("single manager control loop")
    }
    pub(crate) fn connected(&self, address: &str, device: &str, protocol: u32) {
        *self.active.lock().unwrap() = Some(Active {
            address: address.into(),
            device: device.into(),
            protocol,
        });
    }
    pub(crate) fn disconnected(&self) {
        self.prune();
        self.active.lock().unwrap().take();
        for reply in self.pending.lock().unwrap().drain().map(|(_, reply)| reply) {
            let _ = reply.send(Err(ErrorCode::DeviceOffline
                .error("manager relay connection was interrupted")
                .into()));
        }
    }
    pub(crate) fn prune(&self) {
        self.entries
            .lock()
            .unwrap()
            .retain(|_, entry| entry.expires_at > now_ms());
        self.pending
            .lock()
            .unwrap()
            .retain(|_, reply| !reply.is_closed());
    }
    pub(crate) fn acknowledge(&self, id: &str, error: Option<Box<Data>>) {
        if let Some(reply) = self.pending.lock().unwrap().remove(id) {
            let result = match error {
                None => Ok(()),
                Some(error) => match *error {
                    Data::Error { code, message } => {
                        Err(crate::error::CodedError::from_wire(code, message).into())
                    }
                    _ => Err(ErrorCode::InvalidMessage
                        .error("invalid pairing acknowledgement")
                        .into()),
                },
            };
            let _ = reply.send(result);
        }
    }
    pub(crate) fn registrations(&self, address: &str) -> Vec<RelayMessage> {
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, entry| entry.expires_at > now_ms());
        entries
            .values()
            .filter(|entry| entry.address == address)
            .map(|entry| RelayMessage::PairingRegister {
                invitation_id: entry.id.clone(),
                locator: entry.locator.clone(),
                expires_at: entry.expires_at,
            })
            .collect()
    }
    pub(crate) fn entry(&self, id: &str, address: &str) -> Result<Arc<Entry>> {
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, entry| entry.expires_at > now_ms());
        let entry = entries
            .get(id)
            .cloned()
            .context(ErrorCode::CodeExpired.error("access code expired or manager restarted"))?;
        if entry.address != address {
            bail!(ErrorCode::InvalidSession.error("pairing relay differs from invitation"));
        }
        Ok(entry)
    }
    pub(crate) async fn cancel(&self, id: &Identity, invitation: &str) -> Result<()> {
        if !super::valid_id(invitation) {
            bail!(ErrorCode::InvalidRequest.error("invalid invitation identity"));
        }
        network::manager(id)?.cancel_code(invitation)?;
        self.entries.lock().unwrap().remove(invitation);
        self.commands
            .send(Command {
                message: RelayMessage::PairingCancel {
                    invitation_id: invitation.into(),
                },
            })
            .await?;
        Ok(())
    }
    pub(crate) async fn create(
        &self,
        id: &Identity,
        allow: bool,
        replace: Option<&str>,
    ) -> Result<Invitation> {
        self.prune();
        let active = self.active.lock().unwrap().clone().context(
            ErrorCode::DeviceOffline.error("start the manager service and connect its relay first"),
        )?;
        if active.device != id.device_id {
            bail!(ErrorCode::IdentityChanged.error("restart daemon after replacing its identity"));
        }
        ProtocolRange::require(active.protocol, 4)?;
        let manager = network::manager(id)?;
        network::observe(id, &manager.roster()?)?;
        if let Some(old) = replace {
            self.cancel(id, old).await?;
        }
        for _ in 0..16 {
            let invitation_id = uuid::Uuid::new_v4().simple().to_string();
            let locator = random_chars(2);
            let secret = random_chars(6);
            let (token, expires_at) = manager.invite_code(&invitation_id, &locator, allow)?;
            let address = super::relay_origin(&active.address)?;
            let code = format!("{}{}-{}", locator, &secret[..2], &secret[2..]);
            self.entries.lock().unwrap().insert(
                invitation_id.clone(),
                Arc::new(Entry {
                    id: invitation_id.clone(),
                    locator: locator.clone(),
                    secret,
                    token,
                    address: active.address.clone(),
                    expires_at,
                    lease: Arc::new(Semaphore::new(1)),
                }),
            );
            let (reply, accepted) = oneshot::channel();
            self.pending
                .lock()
                .unwrap()
                .insert(invitation_id.clone(), reply);
            self.commands
                .send(Command {
                    message: RelayMessage::PairingRegister {
                        invitation_id: invitation_id.clone(),
                        locator,
                        expires_at,
                    },
                })
                .await?;
            let result = tokio::time::timeout(Duration::from_secs(10), accepted).await;
            let result = match result {
                Ok(Ok(result)) => result,
                _ => Err(ErrorCode::ConnectTimeout
                    .error("relay did not register the invitation")
                    .into()),
            };
            match result {
                Ok(()) => {
                    return Ok(Invitation {
                        link: format!("xrun://{}#{code}", address.trim_start_matches("https://")),
                        allow,
                        expires_in: ((expires_at - now_ms()).max(0) as u64).div_ceil(1000),
                        invitation_id: Some(invitation_id),
                        address: Some(address.trim_start_matches("https://").to_string()),
                        code: Some(code),
                    });
                }
                Err(error) => {
                    self.pending.lock().unwrap().remove(&invitation_id);
                    self.cancel(id, &invitation_id).await?;
                    if !crate::error::is(&error, ErrorCode::PairingBusy) {
                        return Err(error);
                    }
                }
            }
        }
        bail!(ErrorCode::PairingBusy.error("relay pairing slots are busy; try again"))
    }
}
