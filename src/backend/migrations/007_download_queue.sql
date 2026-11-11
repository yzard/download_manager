ALTER TABLE baidu_settings
  ADD COLUMN delete_on_remove INTEGER NOT NULL DEFAULT 0
    CHECK (delete_on_remove IN (0, 1));

CREATE TABLE download_queue_state (
    provider TEXT NOT NULL,
    account_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    cloud_path TEXT NOT NULL,
    disposition TEXT NOT NULL CHECK (disposition IN ('paused', 'removed')),
    PRIMARY KEY (provider, account_id, file_id, cloud_path)
);
