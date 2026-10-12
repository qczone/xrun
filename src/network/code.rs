//! PAKE bootstraps a key-bound strong invitation; ordinary pinned TLS performs enrollment.
use crate::{
    config::{self, Identity},
    crypto,
    error::ErrorCode,
    net,
    pairing::{self, Frame, Hello, channel},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity as PakeIdentity, Password, Spake2};
use std::{path::Path, time::Duration};

#[derive(Serialize, Deserialize)]
struct Pending {
    key_pem: String,
    stamp: String,
    link: Option<String>,
}
fn stamp(key: &str, address: &str, code: &str) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key.as_bytes());
    hex::encode(ring::hmac::sign(
        &key,
        format!("{address}\0{code}").as_bytes(),
    ))
}
fn pending(path: &Path, address: &str, code: &str) -> Result<Pending> {
    let previous: Option<Pending> = if path.exists() {
        Some(config::read(path)?)
    } else {
        None
    };
    if let Some(saved) = previous.as_ref()
        && saved.stamp == stamp(&saved.key_pem, address, code)
    {
        return Ok(Pending {
            key_pem: saved.key_pem.clone(),
            stamp: saved.stamp.clone(),
            link: saved.link.clone(),
        });
    }
    let key_pem = if config::network_dir()?.join("identity.toml").exists() {
        Identity::load()?.key_pem
    } else if config::device_dir()?.join("pending.toml").exists() {
        let pending: config::PendingIdentity =
            config::read(&config::device_dir()?.join("pending.toml"))?;
        pending.key_pem
    } else if let Some(saved) = previous {
        saved.key_pem
    } else {
        crypto::new_device_request()?.0
    };
    Ok(Pending {
        stamp: stamp(&key_pem, address, code),
        key_pem,
        link: None,
    })
}
pub(crate) async fn join(address: &str, code: &str, name: String) -> Result<Identity> {
    if !valid_name(&name) {
        bail!(ErrorCode::InvalidName.error("choose a nonreserved lowercase device name"));
    }
    let address = pairing::origin(address)?;
    let code = pairing::normalize(code)?;
    let directory = config::device_dir()?;
    std::fs::create_dir_all(&directory)?;
    config::restrict_dir(&directory)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("code-pairing.lock"))?;
    lock.try_lock()
        .context(ErrorCode::PairingBusy.error("another local pairing is in progress"))?;
    let path = directory.join("code-pairing.toml");
    let mut pending = pending(&path, &address, &code)?;
    config::write(&path, &pending)?;
    if pending.link.is_none() {
        let link = tokio::time::timeout(
            Duration::from_secs(30),
            bootstrap(&address, &code, &pending.key_pem),
        )
        .await
        .context(ErrorCode::PairingTimeout.error("pairing timed out; retry the same code"))??;
        pending.link = Some(link);
        config::write(&path, &pending)?;
    }
    let id = super::pairing::join_with_key(
        pending.link.as_deref().unwrap(),
        name,
        Some(&pending.key_pem),
    )
    .await?;
    // Keep the private, key-bound receipt for an identical local retry. The
    // manager still rejects revoked keys and cannot enroll a different key.
    Ok(id)
}
async fn bootstrap(address: &str, code: &str, key: &str) -> Result<String> {
    let locator = &code[..2];
    let mut ws = net::websocket_at(
        address,
        &format!("{}{locator}", pairing::PUBLIC_PATH),
        crypto::pairing_tls_config(),
    )
    .await?;
    ProtocolRange::require(ws.get_ref().relay.as_ref().map(|r| r.protocol).unwrap_or(0))?;
    let RelayMessage::PairingInfo {
        invitation_id,
        locator: received,
    } = super::receive(&mut ws).await?
    else {
        bail!(ErrorCode::InvalidMessage.error("missing pairing session identity"));
    };
    if !pairing::valid_id(&invitation_id) || received != locator {
        bail!(ErrorCode::InvalidSession.error("invalid pairing rendezvous"));
    }
    super::authenticate(&mut ws, None, "", "", None).await?;
    if !matches!(
        super::receive(&mut ws).await?,
        RelayMessage::Connected { .. }
    ) {
        bail!(ErrorCode::InvalidMessage.error("pairing tunnel unavailable"));
    }
    let csr = crypto::renew_device_request(key)?;
    let identities = channel::identities(&invitation_id, address, locator, &crypto::csr_key(&csr)?);
    let (state, message) = Spake2::<Ed25519Group>::start_a(
        &Password::new(&code.as_bytes()[2..]),
        &PakeIdentity::new(&identities.0),
        &PakeIdentity::new(&identities.1),
    );
    channel::send(
        &mut ws,
        &Frame::Hello(Hello {
            invitation_id,
            origin: address.into(),
            csr: STANDARD.encode(csr),
            message: STANDARD.encode(&message),
        }),
    )
    .await?;
    let Frame::Challenge { message: remote } = channel::receive(&mut ws).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected pairing challenge"));
    };
    let remote = STANDARD.decode(remote)?;
    channel::validate_message(&remote, b'B')?;
    let key = state
        .finish(&remote)
        .map_err(|_| ErrorCode::InvalidCode.error("access code verification failed"))?;
    let channel = channel::Channel::new(key, &message, &remote, &identities);
    channel::send(
        &mut ws,
        &Frame::Confirm {
            mac: channel.confirm(b"joiner-confirm")?,
        },
    )
    .await?;
    let Frame::Welcome { mac } = channel::receive(&mut ws).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected manager confirmation"));
    };
    channel.verify(b"manager-confirm", &mac)?;
    let Frame::Encrypted { body } = channel::receive(&mut ws).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected encrypted invitation"));
    };
    let link = channel.decrypt(&body)?;
    let invite = super::links::parse_link(&link)?;
    if !invite
        .addresses
        .iter()
        .any(|relay| pairing::relay_origin(relay).is_ok_and(|origin| origin == address))
    {
        bail!(ErrorCode::InvalidRelay.error("verified invitation belongs to another relay"));
    }
    let _ = ws.close(None).await;
    Ok(link)
}
pub(crate) async fn serve(
    id: &Identity,
    invitations: &pairing::Invitations,
    address: &str,
    generation: &str,
    session: &str,
    invitation: &str,
) -> Result<()> {
    let (mut ws, _) = super::attach(id, address, generation, session).await?;
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        serve_inner(id, invitations, address, invitation, &mut ws),
    )
    .await
    .unwrap_or_else(|_| {
        Err(ErrorCode::PairingTimeout
            .error("pairing handshake timed out")
            .into())
    });
    if let Err(error) = &result {
        let _ = channel::send(
            &mut ws,
            &Frame::Error {
                code: crate::error::code(error)
                    .map(|c| c.as_str().to_string())
                    .unwrap_or_else(|| "INVALID_CODE".into()),
                message: "Access code verification did not complete".into(),
            },
        )
        .await;
    }
    net::close(&mut ws).await;
    result
}
async fn serve_inner(
    id: &Identity,
    invitations: &pairing::Invitations,
    address: &str,
    invitation: &str,
    ws: &mut net::Ws,
) -> Result<()> {
    let entry = invitations.entry(invitation, address)?;
    let _lease = entry
        .lease
        .clone()
        .try_acquire_owned()
        .context(ErrorCode::PairingBusy.error("another pairing is in progress"))?;
    let Frame::Hello(hello) = channel::receive(ws).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected pairing hello"));
    };
    if hello.invitation_id != entry.id || hello.origin != pairing::relay_origin(address)? {
        bail!(ErrorCode::InvalidSession.error("pairing context differs from invitation"));
    }
    let manager = super::manager(id)?;
    let csr = STANDARD.decode(&hello.csr)?;
    let key = crypto::csr_key(&csr)?;
    let identities = channel::identities(&entry.id, &hello.origin, &entry.locator, &key);
    let remote = STANDARD.decode(&hello.message)?;
    channel::validate_message(&remote, b'A')?;
    // Malformed CSRs/points never consume the password budget. Charge durably
    // before any response or confirmation can test a guessed password, even
    // when the joiner disconnects instead of sending its confirmation.
    manager.attempt_code(invitation)?;
    let (state, message) = Spake2::<Ed25519Group>::start_b(
        &Password::new(entry.secret.as_bytes()),
        &PakeIdentity::new(&identities.0),
        &PakeIdentity::new(&identities.1),
    );
    let shared = state
        .finish(&remote)
        .map_err(|_| ErrorCode::InvalidCode.error("access code verification failed"))?;
    let channel = channel::Channel::new(shared, &remote, &message, &identities);
    channel::send(
        ws,
        &Frame::Challenge {
            message: STANDARD.encode(&message),
        },
    )
    .await?;
    let Frame::Confirm { mac } = channel::receive(ws).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected joiner confirmation"));
    };
    channel.verify(b"joiner-confirm", &mac)?;
    manager.verify_code(invitation, &key)?;
    channel::send(
        ws,
        &Frame::Welcome {
            mac: channel.confirm(b"manager-confirm")?,
        },
    )
    .await?;
    let link = super::links::invitation_link(&manager.roster()?, &entry.token)?;
    channel::send(
        ws,
        &Frame::Encrypted {
            body: channel.encrypt(&link)?,
        },
    )
    .await?;
    Ok(())
}
