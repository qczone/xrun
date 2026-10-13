//! Durable short-invitation budgets and key binding; no low-entropy secret on disk.
use super::Manager;
use crate::{error::ErrorCode, pairing::MAX_ATTEMPTS, protocol::*};
use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

impl Manager {
    pub(crate) fn invite_code(
        &self,
        id: &str,
        locator: &str,
        allow: bool,
    ) -> Result<(String, i64)> {
        let token = self.invite(allow)?;
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE invitations SET kind='code',invitation_id=?1,locator=?2 WHERE hash=?3",
            params![id, locator, sha256(token.as_bytes())],
        )?;
        let expires = db.query_row(
            "SELECT expires FROM invitations WHERE invitation_id=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok((token, expires))
    }
    pub(crate) fn invalidate_codes(&self) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE invitations SET status='cancelled' WHERE kind='code'",
            [],
        )?;
        Ok(())
    }
    pub(crate) fn cancel_code(&self, id: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE invitations SET status='cancelled' WHERE kind='code' AND invitation_id=?1 AND status!='consumed'",
            [id],
        )?;
        Ok(())
    }
    pub(crate) fn attempt_code(&self, id: &str) -> Result<()> {
        self.attempt_code_at(id, now_ms())
    }
    fn attempt_code_at(&self, id: &str, now: i64) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(i64, i64, String, i64)> = tx
            .query_row(
                "SELECT expires,attempts,status,next_attempt_ms FROM invitations
            WHERE kind='code' AND invitation_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let (expires, attempts, status, next_attempt) =
            row.context(ErrorCode::CodeExpired.error("access code is no longer active"))?;
        if expires <= now || status == "cancelled" {
            bail!(ErrorCode::CodeExpired.error("access code expired; generate another invitation"));
        }
        if status == "consumed" {
            bail!(ErrorCode::CodeInUse.error("access code was already used"));
        }
        if attempts >= MAX_ATTEMPTS {
            bail!(
                ErrorCode::CodeLocked.error(
                    "access code verification budget exhausted; generate another invitation"
                )
            );
        }
        if next_attempt > now {
            bail!(ErrorCode::PairingBusy.error("pairing backoff is active; wait before retrying"));
        }
        tx.execute(
            "UPDATE invitations SET attempts=attempts+1,next_attempt_ms=?2 WHERE invitation_id=?1",
            params![id, now + (1_000_i64 << attempts)],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn verify_code(&self, id: &str, key: &str) -> Result<()> {
        let changed = self.db.lock().unwrap().execute(
            "UPDATE invitations SET status='verified',key_fp=?2
            WHERE invitation_id=?1 AND kind='code' AND status IN ('active','verified')
            AND expires>?3 AND (key_fp IS NULL OR key_fp=?2)",
            params![id, key, now_ms()],
        )?;
        if changed != 1 {
            bail!(
                ErrorCode::CodeInUse
                    .error("invitation is expired or belongs to another device key")
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto;
    use std::sync::Arc;
    #[test]
    fn code_budgets_are_atomic_and_only_the_verified_key_consumes_enrollment() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (manager, _, _, _) = Manager::create(
            dir.path(),
            "manager1",
            vec!["https://relay.example".into()],
            String::new(),
        )?;
        let manager = Arc::new(manager);
        let id = "a".repeat(32);
        manager.invite_code(&id, "K7", false)?;
        let accepted = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..12)
                .map(|_| {
                    let manager = manager.clone();
                    let id = id.clone();
                    scope.spawn(move || manager.attempt_code(&id).is_ok())
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().expect("invitation worker"))
                .filter(|accepted| *accepted)
                .count()
        });
        assert_eq!(accepted, 1);
        let later = now_ms() + 60_000;
        for step in 0..4 {
            manager.attempt_code_at(&id, later + step * 17_000)?;
        }
        let attempts: i64 = manager.db.lock().unwrap().query_row(
            "SELECT attempts FROM invitations WHERE invitation_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        assert_eq!(attempts, 5);
        assert!(crate::error::is(
            &manager.attempt_code(&id).unwrap_err(),
            ErrorCode::CodeLocked
        ));
        let id = "b".repeat(32);
        let (token, _) = manager.invite_code(&id, "M4", true)?;
        manager.attempt_code(&id)?;
        let (_, csr) = crypto::new_device_request()?;
        manager.verify_code(&id, &crypto::csr_key(&csr)?)?;
        let (_, other) = crypto::new_device_request()?;
        assert!(manager.pair(&token, "other1", &other).is_err());
        let paired = manager.pair(&token, "member1", &csr)?;
        assert!(paired.receipt.receipt.allow);
        assert!(crate::error::is(
            &manager.attempt_code(&id).unwrap_err(),
            ErrorCode::CodeInUse
        ));
        assert_eq!(
            manager.pair(&token, "member1", &csr)?.member.device_id,
            paired.member.device_id
        );
        assert_eq!(manager.roster()?.roster.members.len(), 2);
        Ok(())
    }
}
