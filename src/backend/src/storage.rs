pub mod accounts;
pub mod download_queue;
pub mod download_records;

use crate::settings::{BaiduSettings, GeneralSettings, Language};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{fs::OpenOptions, io, os::unix::fs::OpenOptionsExt, path::Path, time::Duration};

const SCHEMA_VERSION: u32 = 8;

const LOAD_BAIDU: &str = "
SELECT bduss
     , stoken
     , monitor_directory
     , check_interval_seconds
     , delete_after_download
     , delete_on_remove
  FROM baidu_settings
 WHERE singleton = 1";

const LOAD_GENERAL: &str = "
SELECT download_directory
     , language
     , base_url
  FROM general_settings
 WHERE singleton = 1";

const SAVE_GENERAL: &str = "
INSERT INTO general_settings (singleton, download_directory, language, base_url)
VALUES (1, ?1, ?2, ?3)
ON CONFLICT (singleton) DO UPDATE
SET download_directory = excluded.download_directory
  , language = excluded.language
  , base_url = excluded.base_url
WHERE ?4";

const SAVE_BAIDU: &str = "
INSERT INTO baidu_settings (
    singleton
  , bduss
  , stoken
  , monitor_directory
  , check_interval_seconds
  , delete_after_download
  , delete_on_remove
)
VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT (singleton) DO UPDATE
SET bduss = excluded.bduss
  , stoken = excluded.stoken
  , monitor_directory = excluded.monitor_directory
  , check_interval_seconds = excluded.check_interval_seconds
  , delete_after_download = excluded.delete_after_download
  , delete_on_remove = excluded.delete_on_remove
WHERE ?7 OR baidu_settings.bduss = '' OR baidu_settings.stoken = ''";

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
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(io::Error::other)?;
    if schema_version(&connection)? != SCHEMA_VERSION {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io::Error::other)?;
        let version = schema_version(&transaction)?;
        match version {
            0 => transaction.execute_batch(include_str!("../schema.sql")),
            1..=7 => {
                for (target, migration) in [
                    (
                        2,
                        include_str!("../migrations/002_account_reset_and_baidu_interval.sql"),
                    ),
                    (3, include_str!("../migrations/003_general_settings.sql")),
                    (4, include_str!("../migrations/004_auth_sessions.sql")),
                    (5, include_str!("../migrations/005_general_base_url.sql")),
                    (
                        6,
                        include_str!("../migrations/006_baidu_delete_after_download.sql"),
                    ),
                    (7, include_str!("../migrations/007_download_queue.sql")),
                    (8, include_str!("../migrations/008_download_records.sql")),
                ] {
                    if version < target {
                        transaction
                            .execute_batch(migration)
                            .map_err(io::Error::other)?;
                    }
                }
                Ok(())
            }
            SCHEMA_VERSION => Ok(()),
            _ => {
                return Err(io::Error::other(
                    "Database schema is newer than this Download Manager version",
                ));
            }
        }
        .map_err(io::Error::other)?;
        if version < SCHEMA_VERSION {
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
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
                delete_after_download: row.get(4)?,
                delete_on_remove: row.get(5)?,
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
                base_url: row.get(2)?,
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
                settings.base_url,
                replace_existing
            ],
        )
        .map(|_| ())
        .map_err(io::Error::other)
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
                settings.delete_after_download,
                settings.delete_on_remove,
                replace_existing
            ],
        )
        .map(|count| count == 1)
        .map_err(io::Error::other)
}

#[cfg(test)]
#[path = "../../../tests/backend/src/storage.rs"]
mod tests;
