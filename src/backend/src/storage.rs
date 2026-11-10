use crate::{
    auth::{Account, AccountAccess},
    settings::{BaiduSettings, GeneralSettings, Language},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{fs::OpenOptions, io, os::unix::fs::OpenOptionsExt, path::Path, time::Duration};

const LOAD_BAIDU: &str = "
SELECT bduss
     , stoken
     , monitor_directory
     , check_interval_seconds
  FROM baidu_settings
 WHERE singleton = 1";

const LOAD_GENERAL: &str = "
SELECT download_directory
     , language
  FROM general_settings
 WHERE singleton = 1";

const SAVE_GENERAL: &str = "
INSERT INTO general_settings (singleton, download_directory, language)
VALUES (1, ?1, ?2)
ON CONFLICT (singleton) DO UPDATE
SET download_directory = excluded.download_directory
  , language = excluded.language
WHERE ?3";

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

const SAVE_BAIDU: &str = "
INSERT INTO baidu_settings (
    singleton
  , bduss
  , stoken
  , monitor_directory
  , check_interval_seconds
)
VALUES (1, ?1, ?2, ?3, ?4)
ON CONFLICT (singleton) DO UPDATE
SET bduss = excluded.bduss
  , stoken = excluded.stoken
  , monitor_directory = excluded.monitor_directory
  , check_interval_seconds = excluded.check_interval_seconds
WHERE ?5 OR baidu_settings.bduss = '' OR baidu_settings.stoken = ''";

fn connect(path: &Path) -> io::Result<Connection> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)?;
    let mut connection = Connection::open(path).map_err(io::Error::other)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(io::Error::other)?;
    if schema_version(&connection)? != 3 {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io::Error::other)?;
        let version = schema_version(&transaction)?;
        match version {
            0 => transaction.execute_batch(include_str!("../schema.sql")),
            1 | 2 => {
                if version == 1 {
                    transaction
                        .execute_batch(include_str!(
                            "../migrations/002_account_reset_and_baidu_interval.sql"
                        ))
                        .map_err(io::Error::other)?;
                }
                transaction.execute_batch(include_str!("../migrations/003_general_settings.sql"))
            }
            3 => Ok(()),
            _ => {
                return Err(io::Error::other(
                    "Database schema is newer than this Download Manager version",
                ));
            }
        }
        .map_err(io::Error::other)?;
        if version < 3 {
            transaction
                .pragma_update(None, "user_version", 3)
                .map_err(io::Error::other)?;
        }
        transaction.commit().map_err(io::Error::other)?;
    }
    Ok(connection)
}

fn schema_version(connection: &Connection) -> io::Result<u32> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(io::Error::other)
}

pub fn load_baidu(path: &Path) -> io::Result<BaiduSettings> {
    Ok(connect(path)?
        .query_row(LOAD_BAIDU, [], |row| {
            Ok(BaiduSettings {
                bduss: row.get(0)?,
                stoken: row.get(1)?,
                monitor_directory: row.get(2)?,
                check_interval_seconds: row.get(3)?,
            })
        })
        .optional()
        .map_err(io::Error::other)?
        .unwrap_or_default())
}

pub fn load_general(path: &Path) -> io::Result<GeneralSettings> {
    Ok(connect(path)?
        .query_row(LOAD_GENERAL, [], |row| {
            let language = match row.get::<_, String>(1)?.as_str() {
                "zh-CN" => Language::Chinese,
                "en" => Language::English,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(GeneralSettings {
                download_directory: row.get(0)?,
                language,
            })
        })
        .optional()
        .map_err(io::Error::other)?
        .unwrap_or_default())
}

pub fn save_general(path: &Path, settings: &GeneralSettings) -> io::Result<()> {
    write_general(&connect(path)?, settings, true)
}

fn write_general(
    connection: &Connection,
    settings: &GeneralSettings,
    replace_existing: bool,
) -> io::Result<()> {
    connection
        .execute(
            SAVE_GENERAL,
            params![
                settings.download_directory,
                settings.language.code(),
                replace_existing
            ],
        )
        .map(|_| ())
        .map_err(io::Error::other)
}

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
    transaction.commit().map_err(io::Error::other)?;
    Ok(AccountAccess {
        account,
        reset_pending,
    })
}

pub fn save_account_setup(path: &Path, account: &Account, reset_pending: bool) -> io::Result<bool> {
    let mut connection = connect(path)?;
    let transaction = connection.transaction().map_err(io::Error::other)?;
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
    transaction.commit().map_err(io::Error::other)?;
    Ok(updated == 1)
}

pub fn change_password(path: &Path, current_hash: &str, new_hash: &str) -> io::Result<()> {
    let updated = connect(path)?
        .execute(CHANGE_PASSWORD, params![new_hash, current_hash])
        .map_err(io::Error::other)?;
    if updated != 1 {
        return Err(io::Error::other("The stored account changed"));
    }
    Ok(())
}

pub fn save_baidu(path: &Path, settings: &BaiduSettings) -> io::Result<()> {
    write_baidu(&connect(path)?, settings, true).map(|_| ())
}

pub fn import_baidu(
    path: &Path,
    baidu: &BaiduSettings,
    general: &GeneralSettings,
) -> io::Result<bool> {
    let mut connection = connect(path)?;
    let transaction = connection.transaction().map_err(io::Error::other)?;
    if !write_baidu(&transaction, baidu, false)? {
        return Ok(false);
    }
    write_general(&transaction, general, false)?;
    transaction.commit().map_err(io::Error::other)?;
    Ok(true)
}

fn write_baidu(
    connection: &Connection,
    settings: &BaiduSettings,
    replace_existing: bool,
) -> io::Result<bool> {
    connection
        .execute(
            SAVE_BAIDU,
            params![
                settings.bduss,
                settings.stoken,
                settings.monitor_directory,
                settings.check_interval_seconds,
                replace_existing
            ],
        )
        .map(|count| count == 1)
        .map_err(io::Error::other)
}

#[cfg(test)]
#[path = "../../../tests/backend/src/storage.rs"]
mod tests;
