ALTER TABLE download_records RENAME TO download_records_v8;

CREATE TABLE download_records (
    provider TEXT NOT NULL,
    account_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    cloud_path TEXT NOT NULL,
    source_size INTEGER NOT NULL CHECK (source_size >= 0),
    total_bytes INTEGER NOT NULL CHECK (total_bytes >= 0),
    downloaded_bytes INTEGER NOT NULL CHECK (downloaded_bytes BETWEEN 0 AND total_bytes),
    completion_reason TEXT CHECK (completion_reason IN ('downloaded', 'already_local', 'remote_deleted', 'remote_moved', 'remote_replaced')),
    completed_at INTEGER,
    CHECK ((completion_reason IS NULL) = (completed_at IS NULL)),
    PRIMARY KEY (provider, account_id, file_id, cloud_path, source_size)
);

INSERT INTO download_records (
    provider, account_id, file_id, cloud_path, source_size,
    total_bytes, downloaded_bytes, completion_reason, completed_at
)
SELECT provider, account_id, file_id, cloud_path, total_bytes,
       total_bytes, downloaded_bytes, completion_reason, completed_at
  FROM download_records_v8;

DROP TABLE download_records_v8;
