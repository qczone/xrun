//! Password-encrypted authority snapshots and atomic activation of validated state.
use super::{
    Manager, RosterCache, SignedRoster,
    records::{PairReceipt, SignedReceipt, sign, verify},
    storage::{save_state, state},
};
use crate::{
    config::{self, DaemonConfig, Identity, NetworkActivation},
    crypto,
    error::ErrorCode,
};
use anyhow::{Context, Result, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::aead;
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8] = b"XRUNAUTH\x01";
const HEADER: usize = MAGIC.len() + 16 + 12;
const MAX_ARCHIVE: usize = 32 * 1024 * 1024;
const MAX_DATABASE: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
/// Nonsecret result of a locally encrypted export or validated restoration.
pub struct BackupInfo {
    /// Network whose signing authority the archive contains.
    pub network_id: String,
    /// Device selected to own the next authority generation.
    pub manager_id: String,
    /// Monotonic management generation, independent of member-list version.
    pub authority_generation: u64,
    /// Version of the signed member list in the snapshot.
    pub roster_version: u64,
    /// Existing member that must import a migration archive, when selected.
    pub transfer_to: Option<String>,
}
impl BackupInfo {
    fn new(roster: &SignedRoster, transfer_to: Option<String>) -> Self {
        Self {
            network_id: roster.roster.network_id.clone(),
            manager_id: roster.roster.manager_id.clone(),
            authority_generation: roster.roster.authority_generation,
            roster_version: roster.roster.version,
            transfer_to,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contents {
    schema: u32,
    identity: Identity,
    config: DaemonConfig,
    root_pem: String,
    root_key: Zeroizing<String>,
    manager_database: String,
    roster: SignedRoster,
    base_roster: SignedRoster,
    transfer_to: Option<String>,
}
impl Drop for Contents {
    fn drop(&mut self) {
        self.identity.key_pem.zeroize();
        self.manager_database.zeroize();
        for value in self.config.env.values_mut() {
            value.zeroize();
        }
    }
}
fn password_key(password: &[u8], salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let text = std::str::from_utf8(password)
        .map_err(|_| ErrorCode::InvalidConfig.error("backup password must be UTF-8"))?;
    if text.chars().count() < 12 || password.len() > 1024 {
        bail!(
            ErrorCode::InvalidConfig
                .error("use a backup password of at least 12 characters, at most 1024 bytes")
        );
    }
    let mut key = Zeroizing::new([0; 32]);
    let params = Params::new(32 * 1024, 3, 1, Some(32))?;
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params).hash_password_into(
        password,
        salt,
        key.as_mut(),
    )?;
    Ok(key)
}
fn cipher(password: &[u8], salt: &[u8]) -> Result<aead::LessSafeKey> {
    let key = password_key(password, salt)?;
    Ok(aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, key.as_ref())
            .map_err(|_| ErrorCode::StorageError.error("backup encryption unavailable"))?,
    ))
}
fn encrypt(contents: &Contents, password: &[u8]) -> Result<Vec<u8>> {
    let mut salt = [0; 16];
    let mut nonce = [0; 12];
    getrandom::fill(&mut salt)?;
    getrandom::fill(&mut nonce)?;
    let header = [MAGIC, &salt, &nonce].concat();
    let mut plain = Zeroizing::new(serde_json::to_vec(contents)?);
    if plain.len() + HEADER + aead::AES_256_GCM.tag_len() > MAX_ARCHIVE {
        bail!(ErrorCode::FileTooLarge.error("authority archive exceeds 32 MiB"));
    }
    cipher(password, &salt)?
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(&header),
            &mut *plain,
        )
        .map_err(|_| ErrorCode::StorageError.error("backup encryption failed"))?;
    Ok([header, plain.to_vec()].concat())
}
fn decrypt(bytes: &[u8], password: &[u8]) -> Result<Contents> {
    if bytes.len() < HEADER + aead::AES_256_GCM.tag_len()
        || bytes.len() > MAX_ARCHIVE
        || !bytes.starts_with(MAGIC)
    {
        bail!(ErrorCode::InvalidConfig.error("unsupported or oversized authority archive"));
    }
    let nonce: [u8; 12] = bytes[MAGIC.len() + 16..HEADER].try_into()?;
    let mut body = Zeroizing::new(bytes[HEADER..].to_vec());
    let plain = cipher(password, &bytes[MAGIC.len()..MAGIC.len() + 16])?
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(&bytes[..HEADER]),
            &mut body,
        )
        .map_err(|_| {
            ErrorCode::BackupAuthenticationFailed
                .error("backup password is incorrect or the archive is damaged")
        })?;
    serde_json::from_slice(plain).map_err(|_| {
        ErrorCode::InvalidConfig
            .error("invalid authority archive contents")
            .into()
    })
}
fn read_bounded(path: &Path, limit: usize) -> Result<Zeroizing<Vec<u8>>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        bail!(ErrorCode::FileTooLarge.error("authority file exceeds its size limit"));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!(ErrorCode::FileTooLarge.error("authority file changed beyond its size limit"));
    }
    Ok(bytes)
}
fn initialize_stage(root: &Path) -> Result<tempfile::TempDir> {
    let stage = tempfile::Builder::new()
        .prefix("xrun-authority-")
        .tempdir_in(root)?;
    config::restrict_dir(stage.path())?;
    Ok(stage)
}
impl Manager {
    fn snapshot(&self, directory: &Path) -> Result<()> {
        std::fs::create_dir_all(directory)?;
        config::restrict_dir(directory)?;
        config::atomic_private_write(&directory.join("root.pem"), self.ca_pem.as_bytes())?;
        config::atomic_private_write(&directory.join("root.key"), self.key_pem.as_bytes())?;
        let path = directory.join("manager.db");
        config::atomic_private_write(&path, b"")?;
        // SQLite backup includes committed WAL pages in one consistent snapshot.
        self.db
            .lock()
            .unwrap()
            .backup(rusqlite::MAIN_DB, &path, None)?;
        // FlushFileBuffers on Windows requires a writable handle.
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)?
            .sync_all()?;
        config::sync_parent(&path)
    }
    pub(crate) fn adopt_roster(&self, next: &SignedRoster) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        state(&tx)?.check_successor(next)?;
        save_state(&tx, next)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn advance_authority(&self, target: &str) -> Result<SignedRoster> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut roster = state(&tx)?;
        let target = roster.member(target)?.clone();
        if target.revoked {
            bail!(ErrorCode::DeviceRevoked.error("management cannot move to a revoked identity"));
        }
        roster.roster.manager_id = target.device_id;
        roster.roster.authority_generation =
            roster.roster.authority_generation.checked_add(1).context(
                ErrorCode::RosterVersionExhausted.error("authority generation exhausted"),
            )?;
        roster.roster.version = roster
            .roster
            .version
            .checked_add(1)
            .context(ErrorCode::RosterVersionExhausted.error("roster version exhausted"))?;
        let roster = SignedRoster::signed(roster.roster, &self.ca_pem, &self.key_pem)?;
        for member in &roster.roster.members {
            let receipt = PairReceipt {
                network_id: roster.roster.network_id.clone(),
                manager_id: roster.roster.manager_id.clone(),
                device_id: member.device_id.clone(),
                allow: false,
            };
            let receipt = SignedReceipt {
                signature: sign(&self.key_pem, "pairing", &receipt)?,
                receipt,
            };
            tx.execute("INSERT INTO receipts VALUES(?1,?2) ON CONFLICT(device) DO UPDATE SET data=excluded.data",
                params![member.device_id, serde_json::to_string(&receipt)?])?;
        }
        tx.execute("DELETE FROM invitations", [])?;
        save_state(&tx, &roster)?;
        tx.commit()?;
        Ok(roster)
    }
}
pub(crate) fn export_at(
    dir: &Path,
    path: &Path,
    password: &[u8],
    transfer: Option<&str>,
) -> Result<BackupInfo> {
    let active = config::network_dir_at(dir)?;
    let identity: Identity = config::read(&active.join("identity.toml"))?;
    let manager = Manager::open(&active.join("manager"))?;
    let roster = manager.roster()?;
    roster.require_manager_identity(&identity)?;
    let stage = tempfile::tempdir()?;
    config::restrict_dir(stage.path())?;
    manager.snapshot(&stage.path().join("manager"))?;
    let snapshot = Manager::open(&stage.path().join("manager"))?;
    let transfer_to = transfer
        .map(|target| {
            snapshot
                .roster()?
                .member(target)
                .map(|member| member.device_id.clone())
        })
        .transpose()?;
    if transfer_to
        .as_ref()
        .is_some_and(|target| target == &identity.device_id)
    {
        bail!(
            ErrorCode::InvalidRequest
                .error("choose another trusted member for management migration")
        );
    }
    let base_roster = snapshot.roster()?;
    let roster = match &transfer_to {
        Some(target) => snapshot.advance_authority(target)?,
        None => snapshot.roster()?,
    };
    // Snapshot again after a staged migration; reading the main DB alone would omit WAL.
    let final_copy = tempfile::tempdir()?;
    config::restrict_dir(final_copy.path())?;
    snapshot.snapshot(&final_copy.path().join("manager"))?;
    let database = read_bounded(&final_copy.path().join("manager/manager.db"), MAX_DATABASE)?;
    let contents = Contents {
        schema: 1,
        identity,
        config: DaemonConfig::load_at(dir)?,
        root_pem: snapshot.ca_pem.clone(),
        root_key: snapshot.key_pem.clone(),
        manager_database: STANDARD.encode(database.as_slice()),
        roster: roster.clone(),
        base_roster,
        transfer_to: transfer_to.clone(),
    };
    let bytes = encrypt(&contents, password)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if parent.canonicalize()?.starts_with(active.canonicalize()?) {
        bail!(
            ErrorCode::InvalidPath
                .error("save the encrypted authority archive outside the active network state")
        );
    }
    let mut output = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    output.write_all(&bytes)?;
    output.as_file().sync_all()?;
    output
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    config::sync_parent(path)?;
    Ok(BackupInfo::new(&roster, transfer_to))
}
fn validate(contents: &Contents, directory: &Path) -> Result<Manager> {
    if contents.schema != 1 {
        bail!(ErrorCode::InvalidConfig.error("unsupported authority archive schema"));
    }
    contents.config.validate()?;
    if contents.root_pem.len() > 64 * 1024
        || contents.root_key.len() > 8 * 1024
        || contents.identity.cert_pem.len() > 64 * 1024
        || contents.identity.key_pem.len() > 8 * 1024
        || contents.identity.ca_pem.len() > 64 * 1024
    {
        bail!(ErrorCode::InvalidConfig.error("oversized authority key or certificate"));
    }
    contents.roster.verify(&contents.roster.roster.network_id)?;
    if contents
        .transfer_to
        .as_deref()
        .unwrap_or(&contents.identity.device_id)
        != contents.roster.roster.manager_id
    {
        bail!(
            ErrorCode::ManagerStateMismatch
                .error("archive management role differs from its signed roster")
        );
    }
    if crypto::cert_der(&contents.root_pem)? != crypto::cert_der(&contents.roster.ca_pem)? {
        bail!(
            ErrorCode::ManagerStateMismatch
                .error("archive root certificate differs from the signed roster")
        );
    }
    verify(
        &contents.root_pem,
        "key-check",
        &"manager",
        &sign(&contents.root_key, "key-check", &"manager")?,
    )?;
    let network = &contents.identity.network;
    if network.network_id != contents.roster.roster.network_id
        || crypto::cert_der(&contents.identity.ca_pem)? != crypto::cert_der(&contents.root_pem)?
    {
        bail!(ErrorCode::NetworkMismatch.error("archive identity belongs to another network"));
    }
    let csr = crypto::renew_device_request(&contents.identity.key_pem)?;
    let member = contents.roster.member(&contents.identity.device_id)?;
    if member.revoked
        || member.key_fp != crypto::csr_key(&csr)?
        || member.name != contents.identity.name
    {
        bail!(
            ErrorCode::IdentityMismatch.error("archive leaf key differs from its member identity")
        );
    }
    let manager_dir = directory.join("manager");
    std::fs::create_dir_all(&manager_dir)?;
    config::restrict_dir(&manager_dir)?;
    let bytes = Zeroizing::new(
        STANDARD
            .decode(&contents.manager_database)
            .map_err(|_| ErrorCode::InvalidConfig.error("invalid authority database encoding"))?,
    );
    if bytes.len() > MAX_DATABASE {
        bail!(ErrorCode::FileTooLarge.error("authority database exceeds 16 MiB"));
    }
    config::atomic_private_write(&manager_dir.join("root.pem"), contents.root_pem.as_bytes())?;
    config::atomic_private_write(&manager_dir.join("root.key"), contents.root_key.as_bytes())?;
    config::atomic_private_write(&manager_dir.join("manager.db"), &bytes)?;
    let db = Connection::open_with_flags(
        manager_dir.join("manager.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let check: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let triggers: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type IN ('trigger','view')",
        [],
        |row| row.get(0),
    )?;
    if check != "ok" || triggers != 0 {
        bail!(ErrorCode::DbCorrupt.error("archive authority database failed validation"));
    }
    drop(db);
    let manager = Manager::open(&manager_dir)?;
    if manager.roster()?.hash()? != contents.roster.hash()? {
        bail!(ErrorCode::ManagerStateMismatch.error("database and signed archive roster differ"));
    }
    Ok(manager)
}
pub(crate) struct PreparedImport {
    manager: Manager,
    contents: Contents,
    // Close SQLite and zeroize secrets before deleting the stage, especially on Windows.
    _validation: tempfile::TempDir,
    _lock: std::fs::File,
    dir: PathBuf,
}
#[cfg(test)]
pub(crate) fn import_at(dir: &Path, path: &Path, password: &[u8]) -> Result<BackupInfo> {
    prepare_import(dir, path, password)?.finish(None)
}
pub(crate) fn prepare_import(dir: &Path, path: &Path, password: &[u8]) -> Result<PreparedImport> {
    let bytes = read_bounded(path, MAX_ARCHIVE)?;
    std::fs::create_dir_all(dir)?;
    config::restrict_dir(dir)?;
    // Never decrypt or stage root keys while business execution is available.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("daemon.lock"))?;
    lock.try_lock().context(
        ErrorCode::DaemonRunning.error("stop the background service before restoring management"),
    )?;
    let tasks = dir.join("daemon.db");
    let unfinished = if tasks.exists() {
        let db = Connection::open_with_flags(&tasks, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.query_row(
            "SELECT COUNT(*) FROM jobs WHERE state IN ('accepted','running')",
            [],
            |row| row.get::<_, i64>(0),
        )?
    } else {
        0
    };
    if unfinished > 0 {
        bail!(ErrorCode::DeviceBusy.error("recover unfinished jobs by starting the service, then stop it before restoring management"));
    }
    let contents = decrypt(&bytes, password)?;
    // All validation runs outside the existing profile, including untrusted SQLite.
    let validation = tempfile::tempdir()?;
    config::restrict_dir(validation.path())?;
    let manager = validate(&contents, validation.path())?;
    contents.base_roster.check_successor(&contents.roster)?;
    Ok(PreparedImport {
        _lock: lock,
        _validation: validation,
        manager,
        contents,
        dir: dir.into(),
    })
}
impl PreparedImport {
    pub(crate) fn base_roster(&self) -> &SignedRoster {
        &self.contents.base_roster
    }
    pub(crate) fn finish(self, latest: Option<SignedRoster>) -> Result<BackupInfo> {
        let Self {
            _lock,
            _validation,
            manager,
            contents,
            dir,
        } = self;
        let dir = dir.as_path();
        if let Some(latest) = latest {
            contents.base_roster.check_successor(&latest)?;
            // The handover snapshot is unpublished; rebase it on the relay's
            // verified current state before advancing the restored generation.
            let mut db = manager.db.lock().unwrap();
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            save_state(&tx, &latest)?;
            tx.commit()?;
        }
        let active = config::network_dir_at(dir)?;
        let previous = if active.join("identity.toml").exists() {
            Some(config::read::<Identity>(&active.join("identity.toml"))?)
        } else {
            None
        };
        let mut identity = match (&contents.transfer_to, &previous) {
            (Some(target), Some(identity)) if &identity.device_id == target => identity.clone(),
            (Some(_), _) => bail!(
                ErrorCode::IdentityMismatch
                    .error("import this migration on its selected member device")
            ),
            (None, _) => contents.identity.clone(),
        };
        if let Some(previous) = &previous {
            if previous.network.network_id != contents.roster.roster.network_id {
                bail!(
                    ErrorCode::NetworkMismatch
                        .error("refusing to replace another network's identity")
                );
            }
            let cache = RosterCache::open(&active.join("roster.db"))?;
            let current = cache.load(&contents.roster.roster.network_id)?;
            let snapshot = manager.roster()?;
            if current.roster.version > snapshot.roster.version
                || current.roster.authority_generation > snapshot.roster.authority_generation
            {
                manager.adopt_roster(&current)?;
            }
        }
        let current = manager.roster()?;
        let member = current.member(&identity.device_id)?;
        if member.revoked
            || member.key_fp != crypto::csr_key(&crypto::renew_device_request(&identity.key_pem)?)?
        {
            bail!(
                ErrorCode::IdentityMismatch
                    .error("restoring device key differs from the current signed roster")
            );
        }
        let roster = manager.advance_authority(&identity.device_id)?;
        identity.cert_pem = manager
            .pair(
                "",
                &identity.name,
                &crypto::renew_device_request(&identity.key_pem)?,
            )?
            .cert_pem;
        identity.network.manager_id = identity.device_id.clone();
        identity.addresses = roster.roster.relay_addresses.clone();
        crypto::verify_member_certificate(
            &identity.cert_pem,
            &identity.ca_pem,
            &identity.device_id,
        )?;
        std::fs::create_dir_all(dir)?;
        config::restrict_dir(dir)?;
        let versions = dir.join("network-versions");
        std::fs::create_dir_all(&versions)?;
        config::restrict_dir(&versions)?;
        let stage = initialize_stage(&versions)?;
        manager.snapshot(&stage.path().join("manager"))?;
        let cache = RosterCache::open(&stage.path().join("roster.db"))?;
        cache.observe(&roster.roster.network_id, &roster)?;
        drop(cache);
        let mut policy = contents.config.clone();
        policy.allow_all = false;
        policy.allow_from.clear();
        policy.pause_generation = policy
            .pause_generation
            .checked_add(1)
            .context(ErrorCode::InvalidConfig.error("pause generation exhausted"))?;
        config::write(&stage.path().join("identity.toml"), &identity)?;
        config::write(&stage.path().join("daemon.toml"), &policy)?;
        let generation = uuid::Uuid::new_v4().simple().to_string();
        let ready = versions.join(&generation);
        std::fs::rename(stage.path(), &ready)?;
        config::sync_parent(&ready)?;
        // One small atomic pointer publishes identity, keys, config and roster together.
        let result = config::atomic_private_write(
            &dir.join("network-state.json"),
            &serde_json::to_vec(&NetworkActivation { generation })?,
        );
        if let Err(error) = result {
            // persist may have succeeded before parent fsync failed. Keep the fully
            // validated generation so a published pointer can never become dangling.
            bail!(ErrorCode::Unconfirmed.error(format!(
                "restoration activation is unconfirmed; validated state retained: {error}"
            )));
        }
        Ok(BackupInfo::new(&roster, contents.transfer_to.clone()))
    }
}

impl SignedRoster {
    fn require_manager_identity(&self, identity: &Identity) -> Result<()> {
        if self.roster.manager_id != identity.device_id {
            bail!(ErrorCode::NotManager.error("only the local manager can export authority"));
        }
        self.peer(
            &crypto::cert_der(&identity.cert_pem)?,
            Some(&identity.device_id),
        )?;
        if self.member(&identity.device_id)?.key_fp
            != crypto::csr_key(&crypto::renew_device_request(&identity.key_pem)?)?
        {
            bail!(
                ErrorCode::IdentityMismatch.error("local manager key differs from its certificate")
            );
        }
        Ok(())
    }
}
fn authority_directories(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut directories = vec![dir.to_owned()];
    match std::fs::read_dir(dir.join("network-versions")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    directories.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(directories)
}
pub(crate) fn holds_authority(dir: &Path) -> Result<bool> {
    Ok(authority_directories(dir)?
        .iter()
        .any(|directory| directory.join("manager/root.key").exists()))
}
pub(crate) fn authority_obsolete(dir: &Path, verified: &SignedRoster) -> Result<bool> {
    let path = config::network_dir_at(dir)?.join("manager/manager.db");
    if !path.exists() {
        return Ok(false);
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let local = state(&db)?;
    local.check_successor(verified)?;
    Ok(local.roster.authority_generation < verified.roster.authority_generation)
}
pub(crate) fn installation_retired(
    dir: &Path,
    identity: &Identity,
    verified: &SignedRoster,
) -> Result<bool> {
    Ok(verified.roster.manager_id == identity.device_id
        && !holds_authority(dir)?
        && authority_obsolete(dir, verified)?)
}
pub(crate) fn retire_at(dir: &Path, roster: &SignedRoster, identity: &Identity) -> Result<()> {
    roster.verify(&roster.roster.network_id)?;
    if identity.network.network_id != roster.roster.network_id {
        bail!(
            ErrorCode::NetworkMismatch
                .error("cannot retire authority using another network's roster")
        );
    }
    if roster.roster.manager_id == identity.device_id && !authority_obsolete(dir, roster)? {
        bail!(
            ErrorCode::ManagerProtected
                .error("the current manager cannot retire its own authority")
        );
    }
    for directory in authority_directories(dir)? {
        let key = directory.join("manager/root.key");
        if key.exists() {
            std::fs::remove_file(&key)?;
            config::sync_parent(&key)?;
        }
    }
    config::update_daemon_config(dir, |policy| {
        policy.allow_all = false;
        policy.allow_from.clear();
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::NetworkIdentity, protocol::Registration};
    const PASSWORD: &[u8] = b"owned-fixture-backup-password";
    fn fixture(dir: &Path) -> Result<(Manager, Identity)> {
        std::fs::create_dir_all(dir)?;
        let (manager, member, key_pem, cert_pem) = Manager::create(
            &dir.join("manager"),
            "manager1",
            vec!["https://relay.example".into()],
            String::new(),
        )?;
        let roster = manager.roster()?;
        let identity = Identity {
            device_id: member.device_id,
            name: member.name,
            key_pem,
            cert_pem,
            ca_pem: roster.ca_pem.clone(),
            addresses: roster.roster.relay_addresses.clone(),
            registration: Registration {
                inviter_id: None,
                allow_inviter: false,
            },
            network: NetworkIdentity {
                network_id: roster.roster.network_id.clone(),
                manager_id: roster.roster.manager_id.clone(),
            },
        };
        config::write(&dir.join("identity.toml"), &identity)?;
        config::write(&dir.join("daemon.toml"), &DaemonConfig::default())?;
        RosterCache::open(&dir.join("roster.db"))?.observe(&roster.roster.network_id, &roster)?;
        Ok((manager, identity))
    }
    #[test]
    fn wrong_password_and_tampering_preserve_all_active_authority_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("profile");
        let (_, identity) = fixture(&dir)?;
        let output = root.path().join("backup.xrun-backup");
        export_at(&dir, &output, PASSWORD, None)?;
        let bytes = std::fs::read(&output)?;
        let key = std::fs::read(dir.join("manager/root.key"))?;
        assert!(!bytes.windows(key.len()).any(|window| window == key));
        let previous = std::fs::read(dir.join("identity.toml"))?;
        assert!(crate::error::is(
            &import_at(&dir, &output, b"wrong-fixture-password").unwrap_err(),
            ErrorCode::BackupAuthenticationFailed
        ));
        let mut damaged = bytes;
        let end = damaged.len() - 1;
        damaged[end] ^= 1;
        std::fs::write(&output, damaged)?;
        assert!(crate::error::is(
            &import_at(&dir, &output, PASSWORD).unwrap_err(),
            ErrorCode::BackupAuthenticationFailed
        ));
        assert_eq!(std::fs::read(dir.join("identity.toml"))?, previous);
        assert_eq!(std::fs::read(dir.join("manager/root.key"))?, key);
        assert!(!dir.join("network-state.json").exists());
        assert_eq!(
            config::read::<Identity>(&dir.join("identity.toml"))?.device_id,
            identity.device_id
        );
        Ok(())
    }
    #[test]
    fn encrypted_wal_snapshot_restores_one_consistent_generation_and_keeps_revocations()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let (manager, identity) = fixture(&source)?;
        let (_, csr) = crypto::new_device_request()?;
        let pair = manager.pair(&manager.invite(false)?, "member1", &csr)?;
        let revoked = manager.revoke(&pair.member.device_id)?;
        RosterCache::open(&source.join("roster.db"))?
            .observe(&revoked.roster.network_id, &revoked)?;
        let output = root.path().join("backup.xrun-backup");
        export_at(&source, &output, PASSWORD, None)?;
        let restored = root.path().join("restored");
        let info = import_at(&restored, &output, PASSWORD)?;
        let active = config::network_dir_at(&restored)?;
        let saved: Identity = config::read(&active.join("identity.toml"))?;
        assert_eq!(saved.device_id, identity.device_id);
        assert_eq!(saved.key_pem, identity.key_pem);
        assert_eq!(info.authority_generation, 2);
        let state = Manager::open(&active.join("manager"))?.roster()?;
        assert!(state.member(&pair.member.device_id)?.revoked);
        assert_eq!(
            state.hash()?,
            RosterCache::open(&active.join("roster.db"))?
                .load(&info.network_id)?
                .hash()?
        );
        assert!(!active.join("daemon-ipc.json").exists());
        assert!(!active.join("cloudflare").exists());
        Ok(())
    }
    #[test]
    fn migration_is_bound_to_the_selected_leaf_key_and_retirement_removes_inactive_copies()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let (manager, identity) = fixture(&source)?;
        let (key_pem, csr) = crypto::new_device_request()?;
        let pair = manager.pair(&manager.invite(false)?, "next-manager", &csr)?;
        let target = root.path().join("target");
        std::fs::create_dir_all(&target)?;
        let next = Identity {
            device_id: pair.member.device_id.clone(),
            name: pair.member.name,
            key_pem,
            cert_pem: pair.cert_pem,
            ca_pem: pair.roster.ca_pem.clone(),
            addresses: pair.roster.roster.relay_addresses.clone(),
            registration: Registration {
                inviter_id: Some(identity.device_id.clone()),
                allow_inviter: false,
            },
            network: NetworkIdentity {
                network_id: pair.roster.roster.network_id.clone(),
                manager_id: identity.device_id.clone(),
            },
        };
        config::write(&target.join("identity.toml"), &next)?;
        config::write(&target.join("daemon.toml"), &DaemonConfig::default())?;
        RosterCache::open(&target.join("roster.db"))?
            .observe(&pair.roster.roster.network_id, &pair.roster)?;
        let output = root.path().join("migration.xrun-backup");
        export_at(&source, &output, PASSWORD, Some(&next.device_id))?;
        assert!(import_at(&root.path().join("wrong-host"), &output, PASSWORD).is_err());
        import_at(&target, &output, PASSWORD)?;
        let active = config::network_dir_at(&target)?;
        let saved: Identity = config::read(&active.join("identity.toml"))?;
        assert_eq!(saved.key_pem, next.key_pem);
        assert_eq!(saved.device_id, next.device_id);
        let published = Manager::open(&active.join("manager"))?.roster()?;
        let copies = source.join("network-versions/unused/manager");
        std::fs::create_dir_all(&copies)?;
        std::fs::copy(source.join("manager/root.key"), copies.join("root.key"))?;
        assert!(holds_authority(&source)?);
        retire_at(&source, &published, &identity)?;
        assert!(!holds_authority(&source)?);
        Ok(())
    }
    #[test]
    fn a_running_daemon_is_rejected_before_decrypting_or_staging_a_root_key() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        fixture(&source)?;
        let output = root.path().join("backup.xrun-backup");
        export_at(&source, &output, PASSWORD, None)?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(source.join("daemon.lock"))?;
        lock.lock()?;
        let error = import_at(&source, &output, PASSWORD).unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::DaemonRunning));
        assert!(!source.join("network-versions").exists());
        Ok(())
    }
}
