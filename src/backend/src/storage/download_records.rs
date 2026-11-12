use super::connect;
use rusqlite::{TransactionBehavior, params};
use serde::Serialize;
use std::{collections::HashMap, io, path::Path};

const LOAD: &str = "
SELECT file_id
     , cloud_path
     , source_size
     , total_bytes
     , downloaded_bytes
     , completion_reason
     , completed_at
  FROM download_records
 WHERE provider = ?1
   AND account_id = ?2";

const SAVE: &str = "
INSERT INTO download_records (
    provider
  , account_id
  , file_id
  , cloud_path
  , source_size
  , total_bytes
  , downloaded_bytes
  , completion_reason
  , completed_at
)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT (provider, account_id, file_id, cloud_path, source_size) DO UPDATE
SET total_bytes = excluded.total_bytes
  , downloaded_bytes = excluded.downloaded_bytes
  , completion_reason = excluded.completion_reason
  , completed_at = excluded.completed_at
WHERE download_records.completion_reason IS NULL";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionReason {
    Downloaded,
    AlreadyLocal,
    RemoteDeleted,
    RemoteMoved,
    RemoteReplaced,
}

impl CompletionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Downloaded => "downloaded",
            Self::AlreadyLocal => "already_local",
            Self::RemoteDeleted => "remote_deleted",
            Self::RemoteMoved => "remote_moved",
            Self::RemoteReplaced => "remote_replaced",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "downloaded" => Ok(Self::Downloaded),
            "already_local" => Ok(Self::AlreadyLocal),
            "remote_deleted" => Ok(Self::RemoteDeleted),
            "remote_moved" => Ok(Self::RemoteMoved),
            "remote_replaced" => Ok(Self::RemoteReplaced),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Completion {
    pub reason: CompletionReason,
    pub at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub total: u64,
    pub downloaded: u64,
    pub completion: Option<Completion>,
}

pub type Records = HashMap<(String, String, u64), Record>;

fn read_bytes(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(row.get::<_, i64>(index)?).map_err(|_| rusqlite::Error::InvalidQuery)
}

pub fn load(db: &Path, provider: &str, account: &str) -> io::Result<Records> {
    let connection = connect(db)?;
    let mut statement = connection.prepare(LOAD).map_err(io::Error::other)?;
    let rows = statement
        .query_map([provider, account], |row| {
            let completion = match row.get::<_, Option<String>>(5)? {
                Some(value) => Some(Completion {
                    reason: CompletionReason::parse(&value)?,
                    at: row.get(6)?,
                }),
                None => None,
            };
            Ok((
                (row.get(0)?, row.get(1)?, read_bytes(row, 2)?),
                Record {
                    total: read_bytes(row, 3)?,
                    downloaded: read_bytes(row, 4)?,
                    completion,
                },
            ))
        })
        .map_err(io::Error::other)?;
    rows.collect::<Result<_, _>>().map_err(io::Error::other)
}

pub fn save(db: &Path, provider: &str, account: &str, records: &Records) -> io::Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let mut connection = connect(db)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(io::Error::other)?;
    {
        let mut statement = transaction.prepare(SAVE).map_err(io::Error::other)?;
        for ((file, path, size), record) in records {
            statement
                .execute(params![
                    provider,
                    account,
                    file,
                    path,
                    i64::try_from(*size).map_err(io::Error::other)?,
                    i64::try_from(record.total).map_err(io::Error::other)?,
                    i64::try_from(record.downloaded).map_err(io::Error::other)?,
                    record.completion.map(|entry| entry.reason.as_str()),
                    record.completion.map(|entry| entry.at)
                ])
                .map_err(io::Error::other)?;
        }
    }
    transaction.commit().map_err(io::Error::other)
}

#[cfg(test)]
#[path = "../../../../tests/backend/src/storage/download_records.rs"]
mod tests;
