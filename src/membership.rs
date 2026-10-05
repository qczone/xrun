//! Network authority lives on the manager. Relays may cache these records, but
//! neither their transport certificate nor their claims establish peer identity.
use crate::{config, crypto, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use ring::{rand::SystemRandom, signature};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path, sync::Mutex};
use time::{Duration, OffsetDateTime};

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
    let mut bytes = format!("xrun/{VERSION}/{domain}\0").into_bytes();
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
    .map_err(|_| anyhow::anyhow!("INVALID_KEY: expected a P-256 private key"))?;
    Ok(STANDARD.encode(
        signer
            .sign(&random, &payload(domain, value)?)
            .map_err(|_| anyhow::anyhow!("SIGNATURE_ERROR: signing failed"))?
            .as_ref(),
    ))
}
pub fn verify<T: Serialize>(
    cert_pem: &str,
    domain: &str,
    value: &T,
    signature: &str,
) -> Result<()> {
    let der = crypto::cert_der(cert_pem)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der)
        .map_err(|e| anyhow::anyhow!("INVALID_CERTIFICATE: {e}"))?;
    signature::UnparsedPublicKey::new(
        &signature::ECDSA_P256_SHA256_ASN1,
        cert.public_key().subject_public_key.data.as_ref(),
    )
    .verify(&payload(domain, value)?, &STANDARD.decode(signature)?)
    .map_err(|_| anyhow::anyhow!("INVALID_SIGNATURE: record signature does not match"))
}
pub fn device_name(id: &str) -> Result<String> {
    if !valid_id(id) {
        bail!("INVALID_DEVICE_ID: expected an immutable device ID")
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
    fn signed(roster: Roster, ca_pem: &str, key: &str) -> Result<Self> {
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
        let r = &self.roster;
        if r.network_id != expected_network
            || r.network_id != format!("net_{}", crypto::ca_spki_pin(&self.ca_pem)?)
        {
            bail!("NETWORK_MISMATCH: roster belongs to another network")
        }
        verify(
            &self.ca_pem,
            "roster",
            &(r, sha256(&crypto::cert_der(&self.ca_pem)?)),
            &self.signature,
        )?;
        if r.version == 0
            || r.members.is_empty()
            || r.members.len() > 256
            || r.relay_addresses.is_empty()
            || r.relay_addresses.len() > 8
        {
            bail!("INVALID_ROSTER: invalid version or member/address count")
        }
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        let mut names = HashSet::new();
        for m in &r.members {
            if !valid_id(&m.device_id)
                || !valid_name(&m.name)
                || m.key_fp.len() != 64
                || !m.key_fp.bytes().all(|b| b.is_ascii_hexdigit())
                || !ids.insert(&m.device_id)
                || !keys.insert(&m.key_fp)
                || (!m.revoked && !names.insert(&m.name))
            {
                bail!("INVALID_ROSTER: invalid or duplicated member")
            }
        }
        if !r
            .members
            .iter()
            .any(|m| m.device_id == r.manager_id && !m.revoked)
        {
            bail!("INVALID_ROSTER: manager must be an active member")
        }
        if !r.relay_ca_pem.is_empty() {
            crypto::cert_der(&r.relay_ca_pem)?;
        }
        for address in &r.relay_addresses {
            let u = url::Url::parse(address)?;
            if u.scheme() != "https"
                || u.host_str().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || !(u.path() == "/" || crate::relay::valid_route(u.path().trim_start_matches('/')))
                || u.query().is_some()
                || u.fragment().is_some()
            {
                bail!(
                    "INVALID_ROSTER: expected HTTPS relay addresses with an optional random route"
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
            .context("UNKNOWN_DEVICE: device is not in the signed roster")
    }
    // Call only after TLS has validated the certificate chain and validity.
    pub fn peer(&self, der: &[u8], expected: Option<&str>) -> Result<&Member> {
        let (id, fp) = crypto::peer_identity(der)?;
        if expected.is_some_and(|target| target != id) {
            bail!("IDENTITY_MISMATCH: unexpected peer device")
        }
        let member = self.member(&id)?;
        if member.key_fp != fp {
            bail!("IDENTITY_MISMATCH: certificate key differs from the roster")
        }
        if member.revoked {
            bail!("DEVICE_REVOKED: peer identity has been revoked")
        }
        Ok(member)
    }
    pub fn check_successor(&self, next: &Self) -> Result<()> {
        next.verify(&self.roster.network_id)?;
        if crypto::cert_der(&next.ca_pem)? != crypto::cert_der(&self.ca_pem)? {
            bail!("INVALID_ROSTER: network root certificate cannot change")
        }
        if next.roster.manager_id != self.roster.manager_id {
            bail!("INVALID_ROSTER: manager identity cannot change")
        }
        if next.roster.version < self.roster.version {
            bail!("ROSTER_ROLLBACK: refusing an older roster")
        }
        if next.roster.version == self.roster.version && next.hash()? != self.hash()? {
            bail!("ROSTER_CONFLICT: same version contains different content")
        }
        for old in &self.roster.members {
            let new = next
                .member(&old.device_id)
                .context("INVALID_ROSTER: members cannot be removed")?;
            if new.key_fp != old.key_fp || new.name != old.name || (old.revoked && !new.revoked) {
                bail!("INVALID_ROSTER: member identity cannot change or reactivate")
            }
        }
        Ok(())
    }
}
impl SignedReceipt {
    pub fn verify(&self, roster: &SignedRoster, expected_device: &str) -> Result<()> {
        let p = &self.receipt;
        if p.network_id != roster.roster.network_id
            || p.manager_id != roster.roster.manager_id
            || p.device_id != expected_device
            || p.device_id == p.manager_id
        {
            bail!("INVALID_RECEIPT: pairing result is not bound to these devices")
        }
        verify(&roster.ca_pem, "pairing", p, &self.signature)?;
        if roster.member(&p.device_id)?.revoked {
            bail!("DEVICE_REVOKED: pairing identity has been revoked")
        }
        Ok(())
    }
}
fn database(path: &Path, create: bool) -> Result<Connection> {
    if !create && !path.exists() {
        bail!("MANAGER_STATE_MISSING: refusing to recreate network authority")
    }
    let parent = path.parent().context("missing database parent")?;
    std::fs::create_dir_all(parent)?;
    config::restrict_dir(parent)?;
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "FULL")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS state(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS invitations(hash TEXT PRIMARY KEY,expires INTEGER NOT NULL,allow INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS receipts(device TEXT PRIMARY KEY,data TEXT NOT NULL);")?;
    Ok(db)
}
fn state(db: &Connection) -> Result<SignedRoster> {
    let data: String = db.query_row("SELECT data FROM state WHERE id=1", [], |r| r.get(0))?;
    Ok(serde_json::from_str(&data)?)
}
fn save_state(db: &Connection, value: &SignedRoster) -> Result<()> {
    db.execute(
        "INSERT INTO state VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
        [serde_json::to_string(value)?],
    )?;
    Ok(())
}

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
            bail!("INVALID_NAME: choose a nonreserved lowercase device name")
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
            .context("MANAGER_BUSY: network creation is already running")?;
        if ["root.pem", "root.key", "manager.db", "device.key.pending"]
            .iter()
            .any(|n| dir.join(n).exists())
        {
            bail!("MANAGER_STATE_EXISTS: refusing to overwrite network authority")
        }
        let root_key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, "xrun network root");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
        params.not_after = OffsetDateTime::now_utc() + Duration::days(7300);
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
            .context("MANAGER_STATE_MISSING: root certificate is missing")?;
        let key_pem = std::fs::read_to_string(dir.join("root.key"))
            .context("MANAGER_STATE_MISSING: root key is missing")?;
        let db = database(&dir.join("manager.db"), false)?;
        let roster = state(&db)?;
        roster.verify(&roster.roster.network_id)?;
        if crypto::ca_spki_pin(&ca_pem)? != crypto::ca_spki_pin(&roster.ca_pem)? {
            bail!("MANAGER_STATE_MISMATCH: root differs from the stored roster")
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
        if count >= 256 {
            bail!("INVITATION_LIMIT: too many active invitations")
        }
        db.execute(
            "INSERT INTO invitations VALUES(?1,?2,?3)",
            params![sha256(token.as_bytes()), now_ms() + 600_000, allow],
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
                bail!("DEVICE_REVOKED: old private keys cannot rejoin")
            }
            member
        } else {
            if !valid_name(name) {
                bail!("INVALID_NAME: choose a nonreserved lowercase device name")
            }
            if roster.roster.members.len() >= 256 {
                bail!("MEMBER_LIMIT: network member limit reached")
            }
            if roster
                .roster
                .members
                .iter()
                .any(|m| m.name == name && !m.revoked)
            {
                bail!("NAME_IN_USE: choose another name")
            }
            let allow: bool = tx
                .query_row(
                    "SELECT allow FROM invitations WHERE hash=?1 AND expires>?2",
                    params![sha256(token.as_bytes()), now_ms()],
                    |r| r.get(0),
                )
                .optional()?
                .context("INVALID_TOKEN: invitation expired or was consumed")?;
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
            roster.roster.version = roster
                .roster
                .version
                .checked_add(1)
                .context("ROSTER_VERSION_EXHAUSTED")?;
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
            bail!("MANAGER_PROTECTED: manager cannot revoke itself")
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
        r.version = r
            .version
            .checked_add(1)
            .context("ROSTER_VERSION_EXHAUSTED")?;
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
        r.version = r
            .version
            .checked_add(1)
            .context("ROSTER_VERSION_EXHAUSTED")?;
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
        .map_err(|e| anyhow::anyhow!("INVALID_CERTIFICATE: {e}"))?;
    p.not_after = (OffsetDateTime::now_utc() + Duration::days(365)).min(
        OffsetDateTime::from_unix_timestamp(root.validity().not_after.timestamp())?,
    );
    if p.not_after <= OffsetDateTime::now_utc() {
        bail!("CA_EXPIRED: network must be recreated")
    }
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    request.params = p;
    Ok(request.signed_by(&issuer)?.pem())
}
pub struct RosterCache(Mutex<Connection>);
impl RosterCache {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self(Mutex::new(database(path, true)?)))
    }
    pub fn load(&self, network: &str) -> Result<SignedRoster> {
        let value = state(&self.0.lock().unwrap())?;
        value.verify(network)?;
        Ok(value)
    }
    pub fn observe(&self, network: &str, next: &SignedRoster) -> Result<()> {
        next.verify(network)?;
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row("SELECT data FROM state WHERE id=1", [], |r| r.get(0))
            .optional()?;
        if let Some(old) = old {
            serde_json::from_str::<SignedRoster>(&old)?.check_successor(next)?;
        }
        save_state(&tx, next)?;
        tx.commit()?;
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
            bail!("INVALID_ACK: acknowledgement is for another roster")
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
