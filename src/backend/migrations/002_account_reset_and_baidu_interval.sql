CREATE TABLE account_reset (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1)
);

ALTER TABLE baidu_settings ADD COLUMN check_interval_seconds INTEGER NOT NULL DEFAULT 300
    CHECK (check_interval_seconds BETWEEN 1 AND 4294967295);
