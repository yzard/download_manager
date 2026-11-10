use crate::{auth::Account, settings::BaiduSettings};
use rusqlite::{Connection, OptionalExtension, params};
use std::{fs::OpenOptions, io, os::unix::fs::OpenOptionsExt, path::Path, time::Duration};

const LOAD_BAIDU: &str = "
SELECT bduss
     , stoken
     , monitor_directory
     , download_directory
  FROM baidu_settings
 WHERE singleton = 1";

const LOAD_ACCOUNT: &str = "
SELECT username
     , password_hash
  FROM allowed_account
 WHERE singleton = 1";

const CREATE_ACCOUNT: &str = "
INSERT INTO allowed_account (singleton, username, password_hash)
VALUES (1, ?1, ?2)
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
  , download_directory
)
VALUES (1, ?1, ?2, ?3, ?4)
ON CONFLICT (singleton) DO UPDATE
SET bduss = excluded.bduss
  , stoken = excluded.stoken
  , monitor_directory = excluded.monitor_directory
  , download_directory = excluded.download_directory
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
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(io::Error::other)?;
    match version {
        0 => {
            let transaction = connection.transaction().map_err(io::Error::other)?;
            transaction
                .execute_batch(include_str!("../schema.sql"))
                .map_err(io::Error::other)?;
            transaction
                .pragma_update(None, "user_version", 1)
                .map_err(io::Error::other)?;
            transaction.commit().map_err(io::Error::other)?;
        }
        1 => {}
        _ => {
            return Err(io::Error::other(
                "Database schema is newer than this Download Manager version",
            ));
        }
    }
    Ok(connection)
}

pub fn load_baidu(path: &Path) -> io::Result<BaiduSettings> {
    Ok(connect(path)?
        .query_row(LOAD_BAIDU, [], |row| {
            Ok(BaiduSettings {
                bduss: row.get(0)?,
                stoken: row.get(1)?,
                monitor_directory: row.get(2)?,
                download_directory: row.get(3)?,
            })
        })
        .optional()
        .map_err(io::Error::other)?
        .unwrap_or_default())
}

pub fn load_account(path: &Path) -> io::Result<Option<Account>> {
    connect(path)?
        .query_row(LOAD_ACCOUNT, [], |row| {
            Ok(Account {
                username: row.get(0)?,
                password_hash: row.get(1)?,
            })
        })
        .optional()
        .map_err(io::Error::other)
}

pub fn create_account(path: &Path, account: &Account) -> io::Result<bool> {
    connect(path)?
        .execute(
            CREATE_ACCOUNT,
            params![account.username, account.password_hash],
        )
        .map(|count| count == 1)
        .map_err(io::Error::other)
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

pub fn save_baidu(
    path: &Path,
    settings: &BaiduSettings,
    replace_existing: bool,
) -> io::Result<bool> {
    connect(path)?
        .execute(
            SAVE_BAIDU,
            params![
                settings.bduss,
                settings.stoken,
                settings.monitor_directory,
                settings.download_directory,
                replace_existing,
            ],
        )
        .map(|count| count == 1)
        .map_err(io::Error::other)
}

#[cfg(test)]
#[path = "../../../tests/backend/src/storage.rs"]
mod tests;
