use super::connect;
use crate::auth::{
    ACCESS_SECONDS, Account, AccountAccess, SESSION_SECONDS, SETUP_CODE_SECONDS, TokenHash,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{io, path::Path};

const LOAD_ACCOUNT: &str = "
SELECT username
     , password_hash
  FROM allowed_account
 WHERE singleton = 1";

const SAVE_ACCOUNT_SETUP: &str = "
INSERT INTO allowed_account (singleton, username, password_hash)
VALUES (1, ?1, ?2)
ON CONFLICT (singleton) DO UPDATE
SET username = excluded.username
  , password_hash = excluded.password_hash
WHERE ?3";

const RESET_CONSUMED: &str = "SELECT EXISTS (SELECT 1 FROM account_reset WHERE singleton = 1)";
const REARM_RESET: &str = "DELETE FROM account_reset WHERE singleton = 1";
const CONSUME_RESET: &str = "
INSERT INTO account_reset (singleton)
VALUES (1)
ON CONFLICT (singleton) DO NOTHING";

const CHANGE_PASSWORD: &str = "
UPDATE allowed_account
   SET password_hash = ?1
 WHERE singleton = 1
   AND password_hash = ?2";

pub fn load_account_access(path: &Path, reset_requested: bool) -> io::Result<AccountAccess> {
    let mut connection = connect(path)?;
    let transaction = connection.transaction().map_err(io::Error::other)?;
    if !reset_requested {
        transaction
            .execute(REARM_RESET, [])
            .map_err(io::Error::other)?;
    }
    let account = transaction
        .query_row(LOAD_ACCOUNT, [], |row| {
            Ok(Account {
                username: row.get(0)?,
                password_hash: row.get(1)?,
            })
        })
        .optional()
        .map_err(io::Error::other)?;
    let reset_pending = reset_requested
        && !transaction
            .query_row(RESET_CONSUMED, [], |row| row.get::<_, bool>(0))
            .map_err(io::Error::other)?;
    if reset_pending {
        transaction
            .execute(DELETE_ALL_SESSIONS, [])
            .map_err(io::Error::other)?;
    } else if account.is_some() {
        transaction
            .execute(DELETE_SETUP_CODE, [])
            .map_err(io::Error::other)?;
    }
    transaction.commit().map_err(io::Error::other)?;
    Ok(AccountAccess {
        account,
        reset_pending,
    })
}

pub fn save_account_setup(
    path: &Path,
    account: &Account,
    reset_pending: bool,
    code: &TokenHash,
    now: i64,
) -> io::Result<bool> {
    let mut connection = connect(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(io::Error::other)?;
    if !check_setup_code(&transaction, code, now)? {
        return Ok(false);
    }
    if reset_pending
        && transaction
            .execute(CONSUME_RESET, [])
            .map_err(io::Error::other)?
            == 0
    {
        return Ok(false);
    }
    let updated = transaction
        .execute(
            SAVE_ACCOUNT_SETUP,
            params![account.username, account.password_hash, reset_pending],
        )
        .map_err(io::Error::other)?;
    if updated != 1 {
        return Ok(false);
    }
    for query in [DELETE_SETUP_CODE, DELETE_ALL_SESSIONS, DELETE_THROTTLE] {
        transaction.execute(query, []).map_err(io::Error::other)?;
    }
    transaction.commit().map_err(io::Error::other)?;
    Ok(true)
}

pub fn change_password(path: &Path, current_hash: &str, new_hash: &str) -> io::Result<()> {
    let mut connection = connect(path)?;
    let transaction = connection.transaction().map_err(io::Error::other)?;
    let updated = transaction
        .execute(CHANGE_PASSWORD, params![new_hash, current_hash])
        .map_err(io::Error::other)?;
    if updated != 1 {
        return Err(io::Error::other("The stored account changed"));
    }
    transaction
        .execute(DELETE_ALL_SESSIONS, [])
        .map_err(io::Error::other)?;
    transaction
        .execute(DELETE_THROTTLE, [])
        .map_err(io::Error::other)?;
    transaction.commit().map_err(io::Error::other)
}

const INSERT_SESSION: &str = "
INSERT INTO login_sessions (refresh_hash, expires_at, secure)
SELECT ?1, ?2, ?3
  FROM allowed_account
 WHERE password_hash = ?4
RETURNING id";

const DELETE_EXPIRED_SESSIONS: &str = "DELETE FROM login_sessions WHERE expires_at <= ?1";
const DELETE_EXPIRED_ACCESS: &str = "DELETE FROM access_tokens WHERE expires_at <= ?1";
const DELETE_ALL_SESSIONS: &str = "DELETE FROM login_sessions";

const TRIM_SESSIONS: &str = "
DELETE FROM login_sessions
 WHERE id IN (
    SELECT id FROM login_sessions ORDER BY expires_at DESC, id DESC LIMIT -1 OFFSET 32
 )";

const TRIM_ACCESS: &str = "
DELETE FROM access_tokens
 WHERE rowid IN (
    SELECT rowid FROM access_tokens
     WHERE session_id = ?1
     ORDER BY expires_at DESC, rowid DESC
     LIMIT -1 OFFSET 8
 )";

const INSERT_ACCESS: &str = "
INSERT INTO access_tokens (token_hash, session_id, expires_at)
VALUES (?1, ?2, ?3)";

const ACTIVE_ACCESS: &str = "
SELECT access.expires_at
  FROM access_tokens AS access
  JOIN login_sessions AS session ON session.id = access.session_id
 WHERE access.token_hash = ?1
   AND session.secure = ?2
   AND access.expires_at > ?3
   AND session.expires_at > ?3";

const ACTIVE_SESSION: &str = "
SELECT id
     , expires_at
  FROM login_sessions
 WHERE refresh_hash = ?1
   AND secure = ?2
   AND expires_at > ?3";

const REVOKE_SESSION: &str = "
DELETE FROM login_sessions
 WHERE secure = ?3
   AND (refresh_hash = ?1 OR id = (SELECT session_id FROM access_tokens WHERE token_hash = ?2))";

const WRITE_SETUP_CODE: &str = "
INSERT INTO account_setup_code (singleton, code_hash, expires_at)
VALUES (1, ?1, ?2)
ON CONFLICT (singleton) DO UPDATE
SET code_hash = excluded.code_hash
  , expires_at = excluded.expires_at";

const ACTIVE_SETUP_CODE: &str = "
SELECT EXISTS (SELECT 1 FROM account_setup_code WHERE code_hash = ?1 AND expires_at > ?2)";

const DELETE_SETUP_CODE: &str = "DELETE FROM account_setup_code";
const DELETE_THROTTLE: &str = "DELETE FROM login_throttle";
const ACCOUNT_EXISTS: &str = "SELECT EXISTS (SELECT 1 FROM allowed_account)";

const LOAD_THROTTLE: &str = "
SELECT failures
     , last_failure
     , retry_after
  FROM login_throttle
 WHERE singleton = 1";

const SAVE_THROTTLE: &str = "
INSERT INTO login_throttle (singleton, failures, last_failure, retry_after)
VALUES (1, ?1, ?2, ?3)
ON CONFLICT (singleton) DO UPDATE
SET failures = excluded.failures
  , last_failure = excluded.last_failure
  , retry_after = excluded.retry_after";

pub fn create_session(
    path: &Path,
    refresh: &TokenHash,
    access: &TokenHash,
    secure: bool,
    now: i64,
    password_hash: &str,
) -> io::Result<()> {
    let mut connection = connect(path)?;
    let tx = connection.transaction().map_err(io::Error::other)?;
    tx.execute(DELETE_EXPIRED_SESSIONS, [now])
        .map_err(io::Error::other)?;
    tx.execute(DELETE_EXPIRED_ACCESS, [now])
        .map_err(io::Error::other)?;
    let id: i64 = tx
        .query_row(
            INSERT_SESSION,
            params![refresh, now + SESSION_SECONDS, secure, password_hash],
            |row| row.get(0),
        )
        .map_err(io::Error::other)?;
    tx.execute(INSERT_ACCESS, params![access, id, now + ACCESS_SECONDS])
        .map_err(io::Error::other)?;
    tx.execute(TRIM_SESSIONS, []).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)
}

pub fn active_access(
    path: &Path,
    access: &TokenHash,
    secure: bool,
    now: i64,
) -> io::Result<Option<i64>> {
    connect(path)?
        .query_row(ACTIVE_ACCESS, params![access, secure, now], |row| {
            row.get(0)
        })
        .optional()
        .map_err(io::Error::other)
}

pub fn renew_access(
    path: &Path,
    refresh: &TokenHash,
    access: &TokenHash,
    secure: bool,
    now: i64,
) -> io::Result<Option<i64>> {
    let mut connection = connect(path)?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(io::Error::other)?;
    let session: Option<(i64, i64)> = tx
        .query_row(ACTIVE_SESSION, params![refresh, secure, now], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()
        .map_err(io::Error::other)?;
    let Some((id, session_expiry)) = session else {
        return Ok(None);
    };
    let expires_at = session_expiry.min(now + ACCESS_SECONDS);
    tx.execute(DELETE_EXPIRED_ACCESS, [now])
        .map_err(io::Error::other)?;
    tx.execute(INSERT_ACCESS, params![access, id, expires_at])
        .map_err(io::Error::other)?;
    tx.execute(TRIM_ACCESS, [id]).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(Some(expires_at))
}

pub fn revoke_session(
    path: &Path,
    refresh: Option<TokenHash>,
    access: Option<TokenHash>,
    secure: bool,
) -> io::Result<()> {
    connect(path)?
        .execute(REVOKE_SESSION, params![refresh, access, secure])
        .map(|_| ())
        .map_err(io::Error::other)
}

pub fn issue_setup_code(
    path: &Path,
    code: &TokenHash,
    reset_requested: bool,
    now: i64,
) -> io::Result<bool> {
    let mut connection = connect(path)?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(io::Error::other)?;
    let account: bool = tx
        .query_row(ACCOUNT_EXISTS, [], |row| row.get(0))
        .map_err(io::Error::other)?;
    let reset_open = reset_requested
        && !tx
            .query_row(RESET_CONSUMED, [], |row| row.get::<_, bool>(0))
            .map_err(io::Error::other)?;
    if account && !reset_open {
        return Ok(false);
    }
    tx.execute(WRITE_SETUP_CODE, params![code, now + SETUP_CODE_SECONDS])
        .map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(true)
}

fn check_setup_code(connection: &Connection, code: &TokenHash, now: i64) -> io::Result<bool> {
    connection
        .query_row(ACTIVE_SETUP_CODE, params![code, now], |row| row.get(0))
        .map_err(io::Error::other)
}

pub fn setup_code_valid(path: &Path, code: &TokenHash, now: i64) -> io::Result<bool> {
    check_setup_code(&connect(path)?, code, now)
}

#[derive(Default)]
pub struct LoginThrottle {
    failures: u32,
    last_failure: i64,
    pub retry_after: i64,
}

impl LoginThrottle {
    pub fn record_failure(self, path: &Path, now: i64) -> io::Result<()> {
        let failures = if now - self.last_failure < 15 * 60 {
            (self.failures + 1).min(6)
        } else {
            1
        };
        let retry_after = now + (1 << (failures - 1));
        connect(path)?
            .execute(SAVE_THROTTLE, params![failures, now, retry_after])
            .map(|_| ())
            .map_err(io::Error::other)
    }
}

pub fn login_throttle(path: &Path) -> io::Result<LoginThrottle> {
    Ok(connect(path)?
        .query_row(LOAD_THROTTLE, [], |row| {
            Ok(LoginThrottle {
                failures: row.get(0)?,
                last_failure: row.get(1)?,
                retry_after: row.get(2)?,
            })
        })
        .optional()
        .map_err(io::Error::other)?
        .unwrap_or_default())
}

pub fn clear_throttle(path: &Path) -> io::Result<()> {
    connect(path)?
        .execute(DELETE_THROTTLE, [])
        .map(|_| ())
        .map_err(io::Error::other)
}

#[cfg(test)]
#[path = "../../../../tests/backend/src/storage/accounts.rs"]
mod tests;
