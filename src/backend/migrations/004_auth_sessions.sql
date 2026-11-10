CREATE TABLE login_sessions (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL DEFAULT 1 REFERENCES allowed_account(singleton) ON DELETE CASCADE,
    refresh_hash BLOB NOT NULL UNIQUE CHECK (length(refresh_hash) = 32),
    expires_at INTEGER NOT NULL,
    secure INTEGER NOT NULL CHECK (secure IN (0, 1))
);

CREATE TABLE access_tokens (
    token_hash BLOB PRIMARY KEY NOT NULL CHECK (length(token_hash) = 32),
    session_id INTEGER NOT NULL REFERENCES login_sessions(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);

CREATE INDEX access_tokens_session_id ON access_tokens (session_id);

CREATE TABLE account_setup_code (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    code_hash BLOB NOT NULL CHECK (length(code_hash) = 32),
    expires_at INTEGER NOT NULL
);

CREATE TABLE login_throttle (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    failures INTEGER NOT NULL CHECK (failures BETWEEN 1 AND 6),
    last_failure INTEGER NOT NULL,
    retry_after INTEGER NOT NULL
);
