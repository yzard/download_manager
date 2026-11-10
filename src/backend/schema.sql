CREATE TABLE IF NOT EXISTS allowed_account (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS account_reset (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1)
);

CREATE TABLE IF NOT EXISTS baidu_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    bduss TEXT NOT NULL,
    stoken TEXT NOT NULL,
    monitor_directory TEXT NOT NULL,
    download_directory TEXT NOT NULL,
    check_interval_seconds INTEGER NOT NULL DEFAULT 300
        CHECK (check_interval_seconds BETWEEN 1 AND 4294967295)
);
