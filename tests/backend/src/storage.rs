use super::accounts::{change_password, issue_setup_code, load_account_access, save_account_setup};
use super::*;
use crate::auth::Account;

#[test]
fn updates_provider_and_account_without_overwriting_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.sqlite3");
    assert!(load_account_access(&path, false).unwrap().account.is_none());
    let mut settings = load_baidu(&path).unwrap();
    settings.bduss = "token-one".to_owned();
    settings.stoken = "token-two".to_owned();
    settings.monitor_directory = "/NAS下载".to_owned();
    settings.check_interval_seconds = 123;

    let account = Account {
        username: "operator".to_owned(),
        password_hash: "original-hash".to_owned(),
    };
    assert!(issue_setup_code(&path, &[1; 32], false, 1).unwrap());
    assert!(save_account_setup(&path, &account, false, &[1; 32], 1).unwrap());
    assert!(!save_account_setup(&path, &account, false, &[1; 32], 1).unwrap());
    let general = GeneralSettings {
        download_directory: "/downloads/shared".to_owned(),
        language: Language::English,
    };
    save_general(&path, &general).unwrap();
    assert!(import_baidu(&path, &settings, &GeneralSettings::default()).unwrap());
    assert_eq!(load_baidu(&path).unwrap().bduss, "token-one");
    assert_eq!(
        load_general(&path).unwrap().download_directory,
        "/downloads/shared"
    );
    assert_eq!(load_baidu(&path).unwrap().check_interval_seconds, 123);

    settings.bduss = "new-token".to_owned();
    assert!(!import_baidu(&path, &settings, &GeneralSettings::default()).unwrap());
    assert_eq!(load_baidu(&path).unwrap().bduss, "token-one");
    change_password(&path, "original-hash", "new-hash").unwrap();
    assert!(change_password(&path, "original-hash", "stale-hash").is_err());
    save_baidu(&path, &settings).unwrap();
    assert_eq!(load_general(&path).unwrap().language.code(), "en");
    assert_eq!(
        load_account_access(&path, false)
            .unwrap()
            .account
            .unwrap()
            .password_hash,
        "new-hash"
    );
    assert_eq!(load_baidu(&path).unwrap().bduss, "new-token");
}

#[test]
fn migrates_version_one_without_losing_account_or_baidu_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.sqlite3");
    let legacy = Connection::open(&path).unwrap();
    legacy
        .execute_batch(
            "
        CREATE TABLE allowed_account (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL
        );
        CREATE TABLE baidu_settings (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            bduss TEXT NOT NULL,
            stoken TEXT NOT NULL,
            monitor_directory TEXT NOT NULL,
            download_directory TEXT NOT NULL
        );
        INSERT INTO allowed_account VALUES (1, 'operator', 'existing-hash');
        INSERT INTO baidu_settings VALUES (1, 'one', 'two', '/NAS下载', '/downloads/archive');
        PRAGMA user_version = 1;
    ",
        )
        .unwrap();
    drop(legacy);
    let settings = load_baidu(&path).unwrap();
    assert_eq!(settings.check_interval_seconds, 300);
    assert_eq!(settings.bduss, "one");
    assert_eq!(settings.stoken, "two");
    assert_eq!(settings.monitor_directory, "/NAS下载");
    let general = load_general(&path).unwrap();
    assert_eq!(general.download_directory, "/downloads/archive");
    assert_eq!(general.language.code(), "zh-CN");
    let access = load_account_access(&path, false).unwrap();
    assert!(access.setup_mode().is_none());
    assert_eq!(access.account.unwrap().password_hash, "existing-hash");
}
