use crate::{auth::Account, settings::Settings};
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

const SAVE_ACCOUNT: &str = "
INSERT INTO allowed_account (singleton, username, password_hash)
VALUES (1, ?1, ?2)
ON CONFLICT(singleton) DO UPDATE
SET username = excluded.username
  , password_hash = excluded.password_hash";

const SAVE_BAIDU: &str = "
INSERT INTO baidu_settings (singleton, bduss, stoken, monitor_directory, download_directory)
VALUES (1, ?1, ?2, ?3, ?4)
ON CONFLICT(singleton) DO UPDATE
SET bduss = excluded.bduss
  , stoken = excluded.stoken
  , monitor_directory = excluded.monitor_directory
  , download_directory = excluded.download_directory";

fn connect(path: &Path) -> io::Result<Connection> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)?;
    let connection = Connection::open(path).map_err(io::Error::other)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(io::Error::other)?;
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(io::Error::other)?;
    if version > 1 {
        return Err(io::Error::other(
            "Database schema is newer than this Download Manager version",
        ));
    }
    connection
        .execute_batch(include_str!("../schema.sql"))
        .map_err(io::Error::other)?;
    connection
        .pragma_update(None, "user_version", 1)
        .map_err(io::Error::other)?;
    Ok(connection)
}

pub fn load(path: &Path) -> io::Result<Settings> {
    let connection = connect(path)?;
    let mut settings = connection
        .query_row(LOAD_BAIDU, [], |row| {
            Ok(Settings {
                bduss: row.get(0)?,
                stoken: row.get(1)?,
                monitor_directory: row.get(2)?,
                download_directory: row.get(3)?,
                account: None,
            })
        })
        .optional()
        .map_err(io::Error::other)?
        .unwrap_or_default();
    settings.account = connection
        .query_row(LOAD_ACCOUNT, [], |row| {
            Ok(Account {
                username: row.get(0)?,
                password_hash: row.get(1)?,
            })
        })
        .optional()
        .map_err(io::Error::other)?;
    Ok(settings)
}

pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    let mut connection = connect(path)?;
    let transaction = connection.transaction().map_err(io::Error::other)?;
    if let Some(account) = &settings.account {
        transaction
            .execute(
                SAVE_ACCOUNT,
                params![account.username, account.password_hash],
            )
            .map_err(io::Error::other)?;
    }
    transaction
        .execute(
            SAVE_BAIDU,
            params![
                settings.bduss,
                settings.stoken,
                settings.monitor_directory,
                settings.download_directory,
            ],
        )
        .map_err(io::Error::other)?;
    transaction.commit().map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_provider_settings_and_rolls_back_failed_account_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.sqlite3");
        let mut settings = load(&path).unwrap();
        assert!(settings.account.is_none());
        settings.bduss = "token-one".to_owned();
        settings.stoken = "token-two".to_owned();
        settings.monitor_directory = "/NAS下载".to_owned();
        settings.account = Some(Account {
            username: "operator".to_owned(),
            password_hash: "original-hash".to_owned(),
        });
        save(&path, &settings).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.bduss, "token-one");
        assert_eq!(loaded.download_directory, "/downloads");
        connect(&path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_settings BEFORE UPDATE ON baidu_settings
             BEGIN SELECT RAISE(ABORT, 'fixture'); END;",
            )
            .unwrap();
        settings.account.as_mut().unwrap().password_hash = "new-hash".to_owned();
        assert!(save(&path, &settings).is_err());
        assert_eq!(
            load(&path).unwrap().account.unwrap().password_hash,
            "original-hash"
        );
    }
}
