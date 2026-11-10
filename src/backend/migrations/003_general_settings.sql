CREATE TABLE general_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    download_directory TEXT NOT NULL,
    language TEXT NOT NULL CHECK (language IN ('zh-CN', 'en'))
);

INSERT INTO general_settings (singleton, download_directory, language)
SELECT singleton
     , download_directory
     , 'zh-CN'
  FROM baidu_settings;

ALTER TABLE baidu_settings DROP COLUMN download_directory;
