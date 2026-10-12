//! Binary-only PAKE records, explicit two-way key confirmation and AEAD binding.
use super::{Frame, MAX_FRAME};
use crate::{error::ErrorCode, net::Ws};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use ring::{aead, hkdf, hmac};
use tokio_tungstenite::tungstenite::Message;

pub(crate) fn identities(
    invitation: &str,
    origin: &str,
    locator: &str,
    key: &str,
) -> (Vec<u8>, Vec<u8>) {
    (
        format!("xrun-pair-v4\0joiner\0{invitation}\0{origin}\0{locator}\0{key}").into_bytes(),
        format!("xrun-pair-v4\0manager\0{invitation}\0{origin}\0{locator}\0{key}").into_bytes(),
    )
}

pub(crate) fn validate_message(message: &[u8], role: u8) -> Result<()> {
    if message.len() != 33 || message[0] != role {
        bail!(ErrorCode::InvalidCode.error("invalid PAKE role or message"));
    }
    let bytes: [u8; 32] = message[1..].try_into()?;
    let point = curve25519_dalek::edwards::CompressedEdwardsY(bytes)
        .decompress()
        .context(ErrorCode::InvalidCode.error("invalid PAKE point"))?;
    if !point.is_torsion_free() || point.is_small_order() || point.compress().to_bytes() != bytes {
        bail!(ErrorCode::InvalidCode.error("invalid PAKE group element"));
    }
    Ok(())
}
pub(crate) async fn send(ws: &mut Ws, frame: &Frame) -> Result<()> {
    let bytes = serde_json::to_vec(frame)?;
    if bytes.len() > MAX_FRAME {
        bail!(ErrorCode::MessageTooLarge.error("pairing record exceeds limit"));
    }
    ws.send(Message::Binary(bytes.into())).await?;
    Ok(())
}
pub(crate) async fn receive(ws: &mut Ws) -> Result<Frame> {
    loop {
        match ws
            .next()
            .await
            .context(ErrorCode::ConnectionClosed.error("pairing connection closed"))??
        {
            Message::Binary(bytes) if bytes.len() <= MAX_FRAME => {
                let frame: Frame = serde_json::from_slice(&bytes)?;
                if let Frame::Error { code, message } = frame {
                    bail!(crate::error::CodedError::from_wire(code, message));
                }
                return Ok(frame);
            }
            Message::Text(text) => {
                if let Ok(crate::protocol::RelayMessage::Error { code, message }) =
                    serde_json::from_str(&text)
                {
                    bail!(crate::error::CodedError::from_wire(code, message));
                }
                if text.len() > 128 {
                    bail!(ErrorCode::InvalidMessage.error("invalid pairing control record"));
                }
                if !serde_json::from_str::<serde_json::Value>(&text)
                    .is_ok_and(|v| v["type"] == "ack" && v["bytes"].as_u64().is_some())
                {
                    bail!(ErrorCode::InvalidMessage.error("expected binary pairing record"));
                }
            }
            Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await?,
            Message::Pong(_) => {}
            _ => bail!(ErrorCode::ConnectionClosed.error("pairing record rejected")),
        }
    }
}
struct KeyLength;
impl hkdf::KeyType for KeyLength {
    fn len(&self) -> usize {
        32
    }
}
pub(crate) struct Channel {
    key: Vec<u8>,
    transcript: Vec<u8>,
}
impl Channel {
    pub(crate) fn new(
        key: Vec<u8>,
        client_message: &[u8],
        server_message: &[u8],
        identities: &(Vec<u8>, Vec<u8>),
    ) -> Self {
        let transcript = serde_json::to_vec(&(
            "xrun-pair-v4",
            &identities.0,
            &identities.1,
            client_message,
            server_message,
        ))
        .expect("byte transcript");
        Self { key, transcript }
    }
    fn derive(&self, purpose: &[u8]) -> Result<[u8; 32]> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &self.transcript).extract(&self.key);
        let info = [purpose];
        let mut key = [0; 32];
        prk.expand(&info, KeyLength)
            .map_err(|_| anyhow::anyhow!("pairing key derivation failed"))?
            .fill(&mut key)
            .map_err(|_| anyhow::anyhow!("pairing key derivation failed"))?;
        Ok(key)
    }
    pub(crate) fn confirm(&self, role: &[u8]) -> Result<String> {
        Ok(STANDARD.encode(hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, &self.derive(role)?),
            &self.transcript,
        )))
    }
    pub(crate) fn verify(&self, role: &[u8], mac: &str) -> Result<()> {
        hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, &self.derive(role)?),
            &self.transcript,
            &STANDARD
                .decode(mac)
                .map_err(|_| ErrorCode::InvalidCode.error("access code verification failed"))?,
        )
        .map_err(|_| ErrorCode::InvalidCode.error("access code verification failed"))?;
        Ok(())
    }
    pub(crate) fn encrypt(&self, value: &str) -> Result<String> {
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.derive(b"invitation")?)
                .map_err(|_| anyhow::anyhow!("pairing encryption unavailable"))?,
        );
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce)?;
        let mut body = value.as_bytes().to_vec();
        key.seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(&self.transcript),
            &mut body,
        )
        .map_err(|_| anyhow::anyhow!("pairing encryption failed"))?;
        Ok(STANDARD.encode([nonce.as_slice(), body.as_slice()].concat()))
    }
    pub(crate) fn decrypt(&self, value: &str) -> Result<String> {
        let mut bytes = STANDARD.decode(value)?;
        if bytes.len() < 28 {
            bail!(ErrorCode::InvalidCode.error("invalid encrypted invitation"));
        }
        let nonce: [u8; 12] = bytes[..12].try_into()?;
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.derive(b"invitation")?)
                .map_err(|_| anyhow::anyhow!("pairing encryption unavailable"))?,
        );
        let plain = key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(&self.transcript),
                &mut bytes[12..],
            )
            .map_err(|_| {
                ErrorCode::InvalidCode.error("encrypted invitation verification failed")
            })?;
        Ok(std::str::from_utf8(plain)?.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spake2::{Ed25519Group, Group, Identity, Password, Spake2};
    #[test]
    fn pake_confirmation_binds_password_roles_and_application_transcript() -> Result<()> {
        assert!(Ed25519Group::const_m().is_torsion_free());
        assert!(Ed25519Group::const_n().is_torsion_free());
        let identities = identities("invitation", "https://relay.example", "K7", "public-key");
        let (a, am) = Spake2::<Ed25519Group>::start_a(
            &Password::new(b"M4P9Q2"),
            &Identity::new(&identities.0),
            &Identity::new(&identities.1),
        );
        let (b, bm) = Spake2::<Ed25519Group>::start_b(
            &Password::new(b"M4P9Q2"),
            &Identity::new(&identities.0),
            &Identity::new(&identities.1),
        );
        validate_message(&am, b'A')?;
        validate_message(&bm, b'B')?;
        let a = Channel::new(a.finish(&bm).unwrap(), &am, &bm, &identities);
        let b = Channel::new(b.finish(&am).unwrap(), &am, &bm, &identities);
        b.verify(b"joiner-confirm", &a.confirm(b"joiner-confirm")?)?;
        a.verify(b"manager-confirm", &b.confirm(b"manager-confirm")?)?;
        assert!(
            a.verify(b"manager-confirm", &b.confirm(b"joiner-confirm")?)
                .is_err()
        );
        let encrypted = b.encrypt("verified network and relay pins")?;
        assert_eq!(a.decrypt(&encrypted)?, "verified network and relay pins");
        let mut tampered = STANDARD.decode(&encrypted)?;
        tampered[15] ^= 1;
        assert!(a.decrypt(&STANDARD.encode(tampered)).is_err());
        assert!(validate_message(&[0; 33], b'A').is_err());
        let (wrong, wm) = Spake2::<Ed25519Group>::start_a(
            &Password::new(b"M4P9Q3"),
            &Identity::new(&identities.0),
            &Identity::new(&identities.1),
        );
        let (server, sm) = Spake2::<Ed25519Group>::start_b(
            &Password::new(b"M4P9Q2"),
            &Identity::new(&identities.0),
            &Identity::new(&identities.1),
        );
        let wrong = Channel::new(wrong.finish(&sm).unwrap(), &wm, &sm, &identities);
        let server = Channel::new(server.finish(&wm).unwrap(), &wm, &sm, &identities);
        assert!(
            server
                .verify(b"joiner-confirm", &wrong.confirm(b"joiner-confirm")?)
                .is_err()
        );
        Ok(())
    }
}
