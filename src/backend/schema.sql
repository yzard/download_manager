CREATE TABLE IF NOT EXISTS allowed_account (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS account_reset (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1)
);

CREATE TABLE IF NOT EXISTS general_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    download_directory TEXT NOT NULL,
    language TEXT NOT NULL CHECK (language IN ('zh-CN', 'en')),
    base_url TEXT NOT NULL DEFAULT '/'
);

CREATE TABLE IF NOT EXISTS baidu_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    bduss TEXT NOT NULL,
    stoken TEXT NOT NULL,
    monitor_directory TEXT NOT NULL,
    check_interval_seconds INTEGER NOT NULL DEFAULT 300
        CHECK (check_interval_seconds BETWEEN 1 AND 4294967295),
    delete_after_download INTEGER NOT NULL DEFAULT 0
        CHECK (delete_after_download IN (0, 1)),
    delete_on_remove INTEGER NOT NULL DEFAULT 0
        CHECK (delete_on_remove IN (0, 1))
);

CREATE TABLE IF NOT EXISTS download_queue_state (
    provider TEXT NOT NULL,
    account_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    cloud_path TEXT NOT NULL,
    disposition TEXT NOT NULL CHECK (disposition IN ('paused', 'removed')),
    PRIMARY KEY (provider, account_id, file_id, cloud_path)
);

CREATE TABLE IF NOT EXISTS download_records (
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

CREATE TABLE IF NOT EXISTS login_sessions (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL DEFAULT 1 REFERENCES allowed_account(singleton) ON DELETE CASCADE,
    refresh_hash BLOB NOT NULL UNIQUE CHECK (length(refresh_hash) = 32),
    expires_at INTEGER NOT NULL,
    secure INTEGER NOT NULL CHECK (secure IN (0, 1))
);

CREATE TABLE IF NOT EXISTS access_tokens (
    token_hash BLOB PRIMARY KEY NOT NULL CHECK (length(token_hash) = 32),
    session_id INTEGER NOT NULL REFERENCES login_sessions(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS access_tokens_session_id ON access_tokens (session_id);

CREATE TABLE IF NOT EXISTS account_setup_code (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    code_hash BLOB NOT NULL CHECK (length(code_hash) = 32),
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS login_throttle (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    failures INTEGER NOT NULL CHECK (failures BETWEEN 1 AND 6),
    last_failure INTEGER NOT NULL,
    retry_after INTEGER NOT NULL
);
