//! Signed membership, pairing permissions and receipt validation.
use crate::{config, crypto, error::ErrorCode, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use rcgen::KeyPair;
use ring::{rand::SystemRandom, signature};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub device_id: String,
    pub name: String,
    pub key_fp: String,
    pub revoked: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    pub network_id: String,
    pub version: u64,
    pub manager_id: String,
    pub members: Vec<Member>,
    pub relay_addresses: Vec<String>,
    /// Empty for public HTTPS relays; otherwise pins the private deployment CA.
    pub relay_ca_pem: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRoster {
    pub roster: Roster,
    pub ca_pem: String,
    pub signature: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairReceipt {
    pub network_id: String,
    pub manager_id: String,
    pub device_id: String,
    pub allow: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedReceipt {
    pub receipt: PairReceipt,
    pub signature: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pairing {
    pub member: Member,
    pub cert_pem: String,
    pub roster: SignedRoster,
    pub receipt: SignedReceipt,
}

// Domain separation prevents a roster signature from being interpreted as a
// pairing permission or a relay authentication proof.
fn payload<T: Serialize>(domain: &str, value: &T) -> Result<Vec<u8>> {
    let mut bytes = format!("xrun/sig-v{SIGNATURE_FORMAT}/{domain}\0").into_bytes();
    bytes.extend(serde_json::to_vec(value)?);
    Ok(bytes)
}
pub fn sign<T: Serialize>(key_pem: &str, domain: &str, value: &T) -> Result<String> {
    let key = KeyPair::from_pem(key_pem)?;
    let random = SystemRandom::new();
    let signer = signature::EcdsaKeyPair::from_pkcs8(
        &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        &key.serialize_der(),
        &random,
    )
    .map_err(|_| anyhow::anyhow!(ErrorCode::InvalidKey.error("expected a P-256 private key")))?;
    Ok(STANDARD.encode(
        signer
            .sign(&random, &payload(domain, value)?)
            .map_err(|_| anyhow::anyhow!(ErrorCode::SignatureError.error("signing failed")))?
            .as_ref(),
    ))
}
pub(crate) fn verify<T: Serialize>(
    cert_pem: &str,
    domain: &str,
    value: &T,
    signature: &str,
) -> Result<()> {
    let der = crypto::cert_der(cert_pem)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der)
        .map_err(|e| anyhow::anyhow!(ErrorCode::InvalidCertificate.error(format!("{e}"))))?;
    signature::UnparsedPublicKey::new(
        &signature::ECDSA_P256_SHA256_ASN1,
        cert.public_key().subject_public_key.data.as_ref(),
    )
    .verify(&payload(domain, value)?, &STANDARD.decode(signature)?)
    .map_err(|_| {
        anyhow::anyhow!(
            ErrorCode::InvalidSignature
                .error(format!("record signature does not match for {domain}"))
        )
    })
}
pub fn device_name(id: &str) -> Result<String> {
    if !valid_id(id) {
        bail!(ErrorCode::InvalidDeviceId.error("expected an immutable device ID"))
    }
    Ok(format!("d{}.xrun", &id[4..]))
}
fn valid_id(id: &str) -> bool {
    id.strip_prefix("dev_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}
impl SignedRoster {
    pub(super) fn signed(roster: Roster, ca_pem: &str, key: &str) -> Result<Self> {
        let signature = sign(
            key,
            "roster",
            &(&roster, sha256(&crypto::cert_der(ca_pem)?)),
        )?;
        let value = Self {
            roster,
            ca_pem: ca_pem.into(),
            signature,
        };
        value.verify(&value.roster.network_id)?;
        Ok(value)
    }
    pub fn hash(&self) -> Result<String> {
        Ok(sha256(&payload(
            "roster",
            &(&self.roster, sha256(&crypto::cert_der(&self.ca_pem)?)),
        )?))
    }
    pub fn verify(&self, expected_network: &str) -> Result<()> {
        let roster = &self.roster;
        if roster.network_id != expected_network
            || roster.network_id != format!("net_{}", crypto::ca_spki_pin(&self.ca_pem)?)
        {
            bail!(ErrorCode::NetworkMismatch.error("roster belongs to another network"))
        }
        verify(
            &self.ca_pem,
            "roster",
            &(roster, sha256(&crypto::cert_der(&self.ca_pem)?)),
            &self.signature,
        )?;
        if roster.version == 0
            || roster.members.is_empty()
            || roster.members.len() > MAX_NETWORK_MEMBERS
            || roster.relay_addresses.is_empty()
            || roster.relay_addresses.len() > MAX_RELAY_ADDRESSES
        {
            bail!(ErrorCode::InvalidRoster.error("invalid version or member/address count"))
        }
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        let mut names = HashSet::new();
        for member in &roster.members {
            if !valid_id(&member.device_id)
                || !valid_name(&member.name)
                || member.key_fp.len() != 64
                || !member.key_fp.bytes().all(|b| b.is_ascii_hexdigit())
                || !ids.insert(&member.device_id)
                || !keys.insert(&member.key_fp)
                || (!member.revoked && !names.insert(&member.name))
            {
                bail!(ErrorCode::InvalidRoster.error("invalid or duplicated member"))
            }
        }
        if !roster
            .members
            .iter()
            .any(|member| member.device_id == roster.manager_id && !member.revoked)
        {
            bail!(ErrorCode::InvalidRoster.error("manager must be an active member"))
        }
        if !roster.relay_ca_pem.is_empty() {
            crypto::cert_der(&roster.relay_ca_pem)?;
        }
        for address in &roster.relay_addresses {
            let url = url::Url::parse(address)?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || !(url.path() == "/"
                    || crate::protocol::valid_relay_route(url.path().trim_start_matches('/')))
                || url.query().is_some()
                || url.fragment().is_some()
            {
                bail!(
                    ErrorCode::InvalidRoster
                        .error("expected HTTPS relay addresses with an optional random route")
                )
            }
        }
        Ok(())
    }
    pub fn member(&self, selector: &str) -> Result<&Member> {
        self.roster
            .members
            .iter()
            .find(|m| m.device_id == selector)
            .or_else(|| {
                self.roster
                    .members
                    .iter()
                    .find(|m| m.name == selector && !m.revoked)
            })
            .or_else(|| self.roster.members.iter().find(|m| m.name == selector))
            .context(ErrorCode::UnknownDevice.error("device is not in the signed roster"))
    }
    // Call only after TLS has validated the certificate chain and validity.
    pub fn peer(&self, der: &[u8], expected: Option<&str>) -> Result<&Member> {
        let (id, fp) = crypto::peer_identity(der)?;
        if expected.is_some_and(|target| target != id) {
            bail!(ErrorCode::IdentityMismatch.error("unexpected peer device"))
        }
        let member = self.member(&id)?;
        if member.key_fp != fp {
            bail!(ErrorCode::IdentityMismatch.error("certificate key differs from the roster"))
        }
        if member.revoked {
            bail!(ErrorCode::DeviceRevoked.error("peer identity has been revoked"))
        }
        Ok(member)
    }
    pub fn check_successor(&self, next: &Self) -> Result<()> {
        next.verify(&self.roster.network_id)?;
        if crypto::cert_der(&next.ca_pem)? != crypto::cert_der(&self.ca_pem)? {
            bail!(ErrorCode::InvalidRoster.error("network root certificate cannot change"))
        }
        if next.roster.manager_id != self.roster.manager_id {
            bail!(ErrorCode::InvalidRoster.error("manager identity cannot change"))
        }
        if next.roster.version < self.roster.version {
            bail!(ErrorCode::RosterRollback.error("refusing an older roster"))
        }
        if next.roster.version == self.roster.version && next.hash()? != self.hash()? {
            bail!(ErrorCode::RosterConflict.error("same version contains different content"))
        }
        for old in &self.roster.members {
            let new = next
                .member(&old.device_id)
                .context(ErrorCode::InvalidRoster.error("members cannot be removed"))?;
            if new.key_fp != old.key_fp || new.name != old.name || (old.revoked && !new.revoked) {
                bail!(ErrorCode::InvalidRoster.error("member identity cannot change or reactivate"))
            }
        }
        Ok(())
    }
}
impl SignedReceipt {
    pub fn verify(&self, roster: &SignedRoster, expected_device: &str) -> Result<()> {
        let receipt = &self.receipt;
        if receipt.network_id != roster.roster.network_id
            || receipt.manager_id != roster.roster.manager_id
            || receipt.device_id != expected_device
            || receipt.device_id == receipt.manager_id
        {
            bail!(ErrorCode::InvalidReceipt.error("pairing result is not bound to these devices"))
        }
        verify(&roster.ca_pem, "pairing", receipt, &self.signature)?;
        if roster.member(&receipt.device_id)?.revoked {
            bail!(ErrorCode::DeviceRevoked.error("pairing identity has been revoked"))
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptAck {
    pub network: String,
    pub device_id: String,
    pub version: u64,
    pub hash: String,
    pub cert_pem: String,
    pub signature: String,
}
impl ReceiptAck {
    fn binding(&self) -> serde_json::Value {
        serde_json::json!({"network":self.network,"device_id":self.device_id,"version":self.version,"hash":self.hash})
    }
    pub fn create(id: &config::Identity, roster: &SignedRoster) -> Result<Self> {
        let mut ack = Self {
            network: roster.roster.network_id.clone(),
            device_id: id.device_id.clone(),
            version: roster.roster.version,
            hash: roster.hash()?,
            cert_pem: id.cert_pem.clone(),
            signature: String::new(),
        };
        ack.signature = sign(&id.key_pem, "roster-ack", &ack.binding())?;
        Ok(ack)
    }
    pub fn verify(&self, roster: &SignedRoster) -> Result<()> {
        if self.network != roster.roster.network_id
            || self.version != roster.roster.version
            || self.hash != roster.hash()?
        {
            bail!(ErrorCode::InvalidAck.error("acknowledgement is for another roster"))
        }
        let der = crypto::cert_der(&self.cert_pem)?;
        crypto::verify_member_certificate(&self.cert_pem, &roster.ca_pem, &self.device_id)?;
        roster.peer(&der, Some(&self.device_id))?;
        verify(
            &self.cert_pem,
            "roster-ack",
            &self.binding(),
            &self.signature,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ChallengeBinding, Proof};

    fn vector() -> serde_json::Value {
        serde_json::from_str(include_str!("../../tests/fixtures/signatures.json")).unwrap()
    }
    fn check<T: Serialize>(fixture: &serde_json::Value, domain: &str, value: &T) -> Result<()> {
        let record = &fixture["records"][domain];
        assert_eq!(
            payload(domain, value)?,
            record["payload_utf8"].as_str().unwrap().as_bytes()
        );
        let member = record["signer"] == "member";
        let certificate = fixture[if member { "member_pem" } else { "root_pem" }]
            .as_str()
            .unwrap();
        let key = fixture[if member {
            "member_private_key_pem"
        } else {
            "root_private_key_pem"
        }]
        .as_str()
        .unwrap();
        verify(
            certificate,
            domain,
            value,
            record["signature"].as_str().unwrap(),
        )?;
        verify(certificate, domain, value, &sign(key, domain, value)?)?;
        assert!(
            verify(
                certificate,
                "wrong-domain",
                value,
                record["signature"].as_str().unwrap()
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn fixed_signed_records_pin_bytes_and_verification_across_releases() -> Result<()> {
        let fixture = vector();
        assert_eq!(fixture["signature_format"], SIGNATURE_FORMAT);
        assert_eq!(
            fixture["protocol"],
            serde_json::to_value(ProtocolRange::CURRENT)?
        );
        let records = &fixture["records"];
        let roster: Roster = serde_json::from_value(records["roster"]["value"][0].clone())?;
        let root_pem = fixture["root_pem"].as_str().unwrap();
        check(
            &fixture,
            "roster",
            &(&roster, sha256(&crypto::cert_der(root_pem)?)),
        )?;
        let signed = SignedRoster {
            roster,
            ca_pem: root_pem.into(),
            signature: records["roster"]["signature"].as_str().unwrap().into(),
        };
        signed.verify(&signed.roster.network_id)?;
        let receipt: PairReceipt = serde_json::from_value(records["pairing"]["value"].clone())?;
        check(&fixture, "pairing", &receipt)?;
        SignedReceipt {
            receipt: receipt.clone(),
            signature: records["pairing"]["signature"].as_str().unwrap().into(),
        }
        .verify(&signed, &receipt.device_id)?;
        let ack_value = &records["roster-ack"]["value"];
        let ack = ReceiptAck {
            network: ack_value["network"].as_str().unwrap().into(),
            device_id: ack_value["device_id"].as_str().unwrap().into(),
            version: ack_value["version"].as_u64().unwrap(),
            hash: ack_value["hash"].as_str().unwrap().into(),
            cert_pem: fixture["member_pem"].as_str().unwrap().into(),
            signature: records["roster-ack"]["signature"].as_str().unwrap().into(),
        };
        check(&fixture, "roster-ack", &ack.binding())?;
        ack.verify(&signed)?;
        let value = &records["relay-proof"]["value"];
        let binding = ChallengeBinding {
            network: value["network"].as_str().unwrap(),
            device: value["device"].as_str().unwrap(),
            path: value["path"].as_str().unwrap(),
            nonce: value["nonce"].as_str().unwrap(),
        };
        check(&fixture, "relay-proof", &binding)?;
        check(&fixture, "relay-manager", &binding)?;
        check(&fixture, "key-check", &"manager")?;
        let proof = Proof {
            device_id: binding.device.into(),
            cert_pem: ack.cert_pem,
            root_pem: root_pem.into(),
            signature: records["relay-proof"]["signature"].as_str().unwrap().into(),
            manager_signature: Some(
                records["relay-manager"]["signature"]
                    .as_str()
                    .unwrap()
                    .into(),
            ),
        };
        assert!(proof.verify(binding.network, binding.path, binding.nonce)?);
        assert!(
            proof
                .verify(binding.network, binding.path, "replay")
                .is_err()
        );
        let mut malformed = serde_json::to_value(proof)?;
        malformed["future_permission"] = true.into();
        assert!(serde_json::from_value::<Proof>(malformed).is_err());
        let mut malformed = serde_json::to_value(signed)?;
        malformed["roster"]["future_permission"] = true.into();
        assert!(serde_json::from_value::<SignedRoster>(malformed).is_err());
        Ok(())
    }
}
