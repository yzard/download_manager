ALTER TABLE baidu_settings
  ADD COLUMN delete_after_download INTEGER NOT NULL DEFAULT 0
    CHECK (delete_after_download IN (0, 1));
