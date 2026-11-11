use super::connect;
use rusqlite::params;
use std::{collections::HashMap, io, path::Path};

const LOAD: &str = "
SELECT file_id
     , cloud_path
     , disposition
  FROM download_queue_state
 WHERE provider = ?1
   AND account_id = ?2";

const SAVE: &str = "
INSERT INTO download_queue_state (provider, account_id, file_id, cloud_path, disposition)
VALUES (?1, ?2, ?3, ?4, ?5)
ON CONFLICT (provider, account_id, file_id, cloud_path) DO UPDATE
SET disposition = excluded.disposition";

const CLEAR: &str = "
DELETE FROM download_queue_state
 WHERE provider = ?1
   AND account_id = ?2
   AND file_id = ?3
   AND cloud_path = ?4";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Paused,
    Removed,
}

pub type Dispositions = HashMap<(String, String), Disposition>;

pub fn load(db: &Path, provider: &str, account: &str) -> io::Result<Dispositions> {
    let connection = connect(db)?;
    let mut statement = connection.prepare(LOAD).map_err(io::Error::other)?;
    let rows = statement
        .query_map([provider, account], |row| {
            let state = match row.get::<_, String>(2)?.as_str() {
                "paused" => Disposition::Paused,
                "removed" => Disposition::Removed,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(((row.get(0)?, row.get(1)?), state))
        })
        .map_err(io::Error::other)?;
    rows.collect::<Result<_, _>>().map_err(io::Error::other)
}

pub fn save(
    db: &Path,
    provider: &str,
    account: &str,
    file: &str,
    path: &str,
    state: Option<Disposition>,
) -> io::Result<()> {
    let connection = connect(db)?;
    match state {
        Some(state) => {
            let value = match state {
                Disposition::Paused => "paused",
                Disposition::Removed => "removed",
            };
            connection.execute(SAVE, params![provider, account, file, path, value])
        }
        None => connection.execute(CLEAR, params![provider, account, file, path]),
    }
    .map(|_| ())
    .map_err(io::Error::other)
}

#[cfg(test)]
#[path = "../../../../tests/backend/src/storage/download_queue.rs"]
mod tests;
