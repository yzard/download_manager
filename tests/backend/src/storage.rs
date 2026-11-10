use super::*;

#[test]
fn updates_provider_and_account_without_overwriting_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.sqlite3");
    assert!(load_account(&path).unwrap().is_none());
    let mut settings = load_baidu(&path).unwrap();
    settings.bduss = "token-one".to_owned();
    settings.stoken = "token-two".to_owned();
    settings.monitor_directory = "/NAS下载".to_owned();

    let account = Account {
        username: "operator".to_owned(),
        password_hash: "original-hash".to_owned(),
    };
    assert!(create_account(&path, &account).unwrap());
    assert!(!create_account(&path, &account).unwrap());
    assert!(save_baidu(&path, &settings, false).unwrap());
    assert_eq!(load_baidu(&path).unwrap().bduss, "token-one");
    assert_eq!(load_baidu(&path).unwrap().download_directory, "/downloads");

    settings.bduss = "new-token".to_owned();
    assert!(!save_baidu(&path, &settings, false).unwrap());
    assert_eq!(load_baidu(&path).unwrap().bduss, "token-one");
    change_password(&path, "original-hash", "new-hash").unwrap();
    assert!(change_password(&path, "original-hash", "stale-hash").is_err());
    assert!(save_baidu(&path, &settings, true).unwrap());
    assert_eq!(
        load_account(&path).unwrap().unwrap().password_hash,
        "new-hash"
    );
    assert_eq!(load_baidu(&path).unwrap().bduss, "new-token");
}
