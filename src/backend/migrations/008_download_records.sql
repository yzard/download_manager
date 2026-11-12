CREATE TABLE download_records (
    provider TEXT NOT NULL,
    account_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    cloud_path TEXT NOT NULL,
    total_bytes INTEGER NOT NULL CHECK (total_bytes >= 0),
    downloaded_bytes INTEGER NOT NULL CHECK (downloaded_bytes BETWEEN 0 AND total_bytes),
    completion_reason TEXT CHECK (completion_reason IN ('downloaded', 'already_local', 'remote_deleted', 'remote_moved')),
    completed_at INTEGER,
    CHECK ((completion_reason IS NULL) = (completed_at IS NULL)),
    PRIMARY KEY (provider, account_id, file_id, cloud_path)
);
