CREATE TABLE IF NOT EXISTS allowed_account (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS baidu_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    bduss TEXT NOT NULL,
    stoken TEXT NOT NULL,
    monitor_directory TEXT NOT NULL,
    download_directory TEXT NOT NULL
);
