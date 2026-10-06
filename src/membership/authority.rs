//! Manager-owned root key, single-use invitations and roster mutation.
use super::{INVITATION_LIFETIME, records::*, storage::*};
use crate::{config, crypto, error::ErrorCode, protocol::*};
use anyhow::{Context, Result, bail};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use std::{path::Path, sync::Mutex};
use time::{Duration, OffsetDateTime};
const MAX_ACTIVE_INVITATIONS: i64 = 256;
const NETWORK_CA_VALIDITY_DAYS: i64 = 20 * 365;

/// One SQLite transaction commits member changes, invitation consumption and
/// the newly signed version together. Concurrent CLI/daemon processes serialize
/// on BEGIN IMMEDIATE rather than each maintaining a version counter.
pub struct Manager {
    db: Mutex<Connection>,
    ca_pem: String,
    key_pem: String,
}
impl Manager {
    pub fn create(
        dir: &Path,
        name: &str,
        relay_addresses: Vec<String>,
        relay_ca_pem: String,
    ) -> Result<(Self, Member, String, String)> {
        if !valid_name(name) {
            bail!(ErrorCode::InvalidName.error("choose a nonreserved lowercase device name"))
        }
        std::fs::create_dir_all(dir)?;
        config::restrict_dir(dir)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("manager.lock"))?;
        lock.try_lock()
            .context(ErrorCode::ManagerBusy.error("network creation is already running"))?;
        if ["root.pem", "root.key", "manager.db", "device.key.pending"]
            .iter()
            .any(|n| dir.join(n).exists())
        {
            bail!(ErrorCode::ManagerStateExists.error("refusing to overwrite network authority"))
        }
        let root_key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, "xrun network root");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
        params.not_after = OffsetDateTime::now_utc() + Duration::days(NETWORK_CA_VALIDITY_DAYS);
        let ca_pem = params.self_signed(&root_key)?.pem();
        let key_pem = root_key.serialize_pem();
        let (device_key, csr) = crypto::new_device_request()?;
        let member = Member {
            device_id: format!("dev_{}", uuid::Uuid::new_v4().simple()),
            name: name.into(),
            key_fp: crypto::csr_key(&csr)?,
            revoked: false,
        };
        let roster = SignedRoster::signed(
            Roster {
                network_id: format!("net_{}", crypto::ca_spki_pin(&ca_pem)?),
                version: 1,
                manager_id: member.device_id.clone(),
                members: vec![member.clone()],
                relay_addresses,
                relay_ca_pem,
            },
            &ca_pem,
            &key_pem,
        )?;
        let cert = issue(&ca_pem, &key_pem, &csr, &member.device_id)?;
        // Partial creation is deliberately an error on the next attempt; never
        // silently mint another authority over an existing member database.
        config::atomic_private_write(&dir.join("device.key.pending"), device_key.as_bytes())?;
        config::atomic_private_write(&dir.join("device.pem.pending"), cert.as_bytes())?;
        config::atomic_private_write(&dir.join("root.key"), key_pem.as_bytes())?;
        config::atomic_private_write(&dir.join("root.pem"), ca_pem.as_bytes())?;
        let mut db = database(&dir.join("manager.db"), true)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        save_state(&tx, &roster)?;
        tx.commit()?;
        Ok((
            Self {
                db: Mutex::new(db),
                ca_pem,
                key_pem,
            },
            member,
            device_key,
            cert,
        ))
    }
    pub fn open(dir: &Path) -> Result<Self> {
        let ca_pem = std::fs::read_to_string(dir.join("root.pem"))
            .context(ErrorCode::ManagerStateMissing.error("root certificate is missing"))?;
        let key_pem = std::fs::read_to_string(dir.join("root.key"))
            .context(ErrorCode::ManagerStateMissing.error("root key is missing"))?;
        let db = database(&dir.join("manager.db"), false)?;
        let roster = state(&db)?;
        roster.verify(&roster.roster.network_id)?;
        if crypto::ca_spki_pin(&ca_pem)? != crypto::ca_spki_pin(&roster.ca_pem)? {
            bail!(ErrorCode::ManagerStateMismatch.error("root differs from the stored roster"))
        }
        verify(
            &ca_pem,
            "key-check",
            &"manager",
            &sign(&key_pem, "key-check", &"manager")?,
        )?;
        Ok(Self {
            db: Mutex::new(db),
            ca_pem,
            key_pem,
        })
    }
    pub fn roster(&self) -> Result<SignedRoster> {
        state(&self.db.lock().unwrap())
    }
    /// Proves to the relay that this device holds the network root key.
    pub(crate) fn sign_relay<T: Serialize>(&self, value: &T) -> Result<String> {
        sign(&self.key_pem, "relay-manager", value)
    }
    pub fn invite(&self, allow: bool) -> Result<String> {
        let token = crypto::random_token();
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM invitations WHERE expires<=?1", [now_ms()])?;
        let count: i64 = db.query_row("SELECT COUNT(*) FROM invitations", [], |r| r.get(0))?;
        if count >= MAX_ACTIVE_INVITATIONS {
            bail!(ErrorCode::InvitationLimit.error("too many active invitations"))
        }
        db.execute(
            "INSERT INTO invitations VALUES(?1,?2,?3)",
            params![
                sha256(token.as_bytes()),
                now_ms() + INVITATION_LIFETIME.as_millis() as i64,
                allow
            ],
        )?;
        Ok(token)
    }
    pub fn pair(&self, token: &str, name: &str, csr: &[u8]) -> Result<Pairing> {
        let fp = crypto::csr_key(csr)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut roster = state(&tx)?;
        let existing = roster
            .roster
            .members
            .iter()
            .find(|m| m.key_fp == fp)
            .cloned();
        let member = if let Some(member) = existing {
            if member.revoked {
                bail!(ErrorCode::DeviceRevoked.error("old private keys cannot rejoin"))
            }
            member
        } else {
            if !valid_name(name) {
                bail!(ErrorCode::InvalidName.error("choose a nonreserved lowercase device name"))
            }
            if roster.roster.members.len() >= MAX_NETWORK_MEMBERS {
                bail!(ErrorCode::MemberLimit.error("network member limit reached"))
            }
            if roster
                .roster
                .members
                .iter()
                .any(|m| m.name == name && !m.revoked)
            {
                bail!(ErrorCode::NameInUse.error("choose another name"))
            }
            let allow: bool = tx
                .query_row(
                    "SELECT allow FROM invitations WHERE hash=?1 AND expires>?2",
                    params![sha256(token.as_bytes()), now_ms()],
                    |r| r.get(0),
                )
                .optional()?
                .context(ErrorCode::InvalidToken.error("invitation expired or was consumed"))?;
            let member = Member {
                device_id: format!("dev_{}", uuid::Uuid::new_v4().simple()),
                name: name.into(),
                key_fp: fp,
                revoked: false,
            };
            let receipt = PairReceipt {
                network_id: roster.roster.network_id.clone(),
                manager_id: roster.roster.manager_id.clone(),
                device_id: member.device_id.clone(),
                allow,
            };
            let receipt = SignedReceipt {
                signature: sign(&self.key_pem, "pairing", &receipt)?,
                receipt,
            };
            tx.execute(
                "INSERT INTO receipts VALUES(?1,?2)",
                params![member.device_id, serde_json::to_string(&receipt)?],
            )?;
            tx.execute(
                "DELETE FROM invitations WHERE hash=?1",
                [sha256(token.as_bytes())],
            )?;
            roster.roster.members.push(member.clone());
            roster.roster.version = roster.roster.version.checked_add(1).context(
                ErrorCode::RosterVersionExhausted.error("membership version cannot be incremented"),
            )?;
            roster = SignedRoster::signed(roster.roster, &self.ca_pem, &self.key_pem)?;
            save_state(&tx, &roster)?;
            member
        };
        let receipt = if member.device_id == roster.roster.manager_id {
            let receipt = PairReceipt {
                network_id: roster.roster.network_id.clone(),
                manager_id: member.device_id.clone(),
                device_id: member.device_id.clone(),
                allow: false,
            };
            SignedReceipt {
                signature: sign(&self.key_pem, "pairing", &receipt)?,
                receipt,
            }
        } else {
            let data: String = tx.query_row(
                "SELECT data FROM receipts WHERE device=?1",
                [&member.device_id],
                |r| r.get(0),
            )?;
            serde_json::from_str(&data)?
        };
        let cert_pem = issue(&self.ca_pem, &self.key_pem, csr, &member.device_id)?;
        tx.commit()?;
        Ok(Pairing {
            member,
            cert_pem,
            roster,
            receipt,
        })
    }
    pub fn revoke(&self, selector: &str) -> Result<SignedRoster> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = state(&tx)?;
        let target = old.member(selector)?;
        if target.device_id == old.roster.manager_id {
            bail!(ErrorCode::ManagerProtected.error("manager cannot revoke itself"))
        }
        if target.revoked {
            return Ok(old);
        }
        let id = target.device_id.clone();
        let mut r = old.roster;
        r.members
            .iter_mut()
            .find(|m| m.device_id == id)
            .unwrap()
            .revoked = true;
        r.version = r.version.checked_add(1).context(
            ErrorCode::RosterVersionExhausted.error("membership version cannot be incremented"),
        )?;
        let next = SignedRoster::signed(r, &self.ca_pem, &self.key_pem)?;
        save_state(&tx, &next)?;
        tx.commit()?;
        Ok(next)
    }
    pub fn set_relay(&self, addresses: Vec<String>, ca_pem: String) -> Result<SignedRoster> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut r = state(&tx)?.roster;
        r.relay_addresses = addresses;
        r.relay_ca_pem = ca_pem;
        r.version = r.version.checked_add(1).context(
            ErrorCode::RosterVersionExhausted.error("membership version cannot be incremented"),
        )?;
        let next = SignedRoster::signed(r, &self.ca_pem, &self.key_pem)?;
        save_state(&tx, &next)?;
        tx.commit()?;
        Ok(next)
    }
}
fn issue(ca: &str, root_key: &str, csr: &[u8], id: &str) -> Result<String> {
    let key = KeyPair::from_pem(root_key)?;
    let issuer = Issuer::from_ca_cert_pem(ca, &key)?;
    let mut request = CertificateSigningRequestParams::from_der(&csr.into())?;
    let mut p = CertificateParams::new(vec![device_name(id)?])?;
    p.distinguished_name.push(DnType::CommonName, id);
    p.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    let der = crypto::cert_der(ca)?;
    let (_, root) = x509_parser::parse_x509_certificate(&der)
        .map_err(|e| anyhow::anyhow!(ErrorCode::InvalidCertificate.error(format!("{e}"))))?;
    p.not_after = (OffsetDateTime::now_utc() + Duration::days(365)).min(
        OffsetDateTime::from_unix_timestamp(root.validity().not_after.timestamp())?,
    );
    if p.not_after <= OffsetDateTime::now_utc() {
        bail!(ErrorCode::CaExpired.error("network must be recreated"))
    }
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    request.params = p;
    Ok(request.signed_by(&issuer)?.pem())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhausted_roster_version_is_typed_and_rolls_back() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let manager = Manager::create(
            dir.path(),
            "manager1",
            vec!["https://relay.example.com".into()],
            String::new(),
        )?
        .0;
        let mut roster = manager.roster()?.roster;
        roster.version = u64::MAX;
        let signed = SignedRoster::signed(roster, &manager.ca_pem, &manager.key_pem)?;
        save_state(&manager.db.lock().unwrap(), &signed)?;
        let error = manager
            .set_relay(vec!["https://other.example.com".into()], String::new())
            .unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::RosterVersionExhausted));
        assert_eq!(manager.roster()?.roster.version, u64::MAX);
        Ok(())
    }
}
