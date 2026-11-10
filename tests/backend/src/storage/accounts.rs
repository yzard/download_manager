use super::*;

fn account(username: &str) -> Account {
    Account {
        username: username.to_owned(),
        password_hash: "test-hash".to_owned(),
    }
}

#[test]
fn persists_sessions_and_enforces_both_lifetimes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.sqlite3");
    let (refresh, access, replacement) = ([1; 32], [2; 32], [3; 32]);
    let start = 1_000;
    assert!(issue_setup_code(&path, &[9; 32], false, start).unwrap());
    assert!(save_account_setup(&path, &account("operator"), false, &[9; 32], start).unwrap());
    create_session(&path, &refresh, &access, true, start, "test-hash").unwrap();
    assert_eq!(
        active_access(&path, &access, true, start + 1).unwrap(),
        Some(start + ACCESS_SECONDS)
    );
    assert_eq!(active_access(&path, &access, false, start).unwrap(), None);
    let rotation = start + ACCESS_SECONDS;
    assert_eq!(active_access(&path, &access, true, rotation).unwrap(), None);
    assert_eq!(
        renew_access(&path, &refresh, &replacement, false, rotation).unwrap(),
        None
    );
    assert_eq!(
        renew_access(&path, &refresh, &replacement, true, rotation).unwrap(),
        Some(rotation + ACCESS_SECONDS)
    );
    assert_eq!(
        active_access(&path, &replacement, true, rotation).unwrap(),
        Some(rotation + ACCESS_SECONDS)
    );
    let expiry = start + SESSION_SECONDS;
    assert_eq!(
        renew_access(&path, &refresh, &[4; 32], true, expiry - 1).unwrap(),
        Some(expiry)
    );
    assert_eq!(
        renew_access(&path, &refresh, &[5; 32], true, expiry).unwrap(),
        None
    );
    assert_eq!(active_access(&path, &[4; 32], true, expiry).unwrap(), None);
}

#[test]
fn logout_and_password_change_revoke_access_and_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.sqlite3");
    assert!(issue_setup_code(&path, &[1; 32], false, 10).unwrap());
    assert!(save_account_setup(&path, &account("operator"), false, &[1; 32], 10).unwrap());

    create_session(&path, &[1; 32], &[2; 32], true, 10, "test-hash").unwrap();
    create_session(&path, &[3; 32], &[4; 32], true, 10, "test-hash").unwrap();
    revoke_session(&path, Some([1; 32]), None, true).unwrap();
    assert_eq!(active_access(&path, &[2; 32], true, 11).unwrap(), None);
    assert_eq!(
        renew_access(&path, &[1; 32], &[5; 32], true, 11).unwrap(),
        None
    );
    assert!(active_access(&path, &[4; 32], true, 11).unwrap().is_some());

    change_password(&path, "test-hash", "new-hash").unwrap();
    assert_eq!(active_access(&path, &[4; 32], true, 11).unwrap(), None);
    assert_eq!(
        renew_access(&path, &[3; 32], &[6; 32], true, 11).unwrap(),
        None
    );
}

#[test]
fn setup_codes_expire_and_reset_revokes_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.sqlite3");
    let (first, reset) = ([1; 32], [2; 32]);
    let initial = account("operator");

    assert!(issue_setup_code(&path, &first, false, 10).unwrap());
    assert!(!save_account_setup(&path, &initial, false, &reset, 10).unwrap());
    assert!(!save_account_setup(&path, &initial, false, &first, 10 + SETUP_CODE_SECONDS).unwrap());
    assert!(issue_setup_code(&path, &first, false, 1000).unwrap());
    assert!(save_account_setup(&path, &initial, false, &first, 1000).unwrap());
    assert!(!issue_setup_code(&path, &first, false, 1001).unwrap());
    assert!(!setup_code_valid(&path, &first, 1001).unwrap());

    create_session(&path, &[3; 32], &[4; 32], false, 1001, "test-hash").unwrap();
    assert!(load_account_access(&path, true).unwrap().reset_pending);
    assert_eq!(
        renew_access(&path, &[3; 32], &[5; 32], false, 1002).unwrap(),
        None
    );
    assert!(issue_setup_code(&path, &reset, true, 1002).unwrap());
    let replacement = account("replacement");
    let outcomes = std::thread::scope(|scope| {
        let first =
            scope.spawn(|| save_account_setup(&path, &replacement, true, &reset, 1002).unwrap());
        let second =
            scope.spawn(|| save_account_setup(&path, &replacement, true, &reset, 1002).unwrap());
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(outcomes.into_iter().filter(|updated| *updated).count(), 1);
    assert!(!load_account_access(&path, true).unwrap().reset_pending);
    assert!(!issue_setup_code(&path, &reset, true, 1003).unwrap());
    assert!(!load_account_access(&path, false).unwrap().reset_pending);
    assert!(load_account_access(&path, true).unwrap().reset_pending);
}

#[test]
fn keeps_login_throttle_after_reopening_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.sqlite3");
    login_throttle(&path)
        .unwrap()
        .record_failure(&path, 1000)
        .unwrap();
    assert_eq!(login_throttle(&path).unwrap().retry_after, 1001);
    login_throttle(&path)
        .unwrap()
        .record_failure(&path, 1001)
        .unwrap();
    assert_eq!(login_throttle(&path).unwrap().retry_after, 1003);
    clear_throttle(&path).unwrap();
    assert_eq!(login_throttle(&path).unwrap().retry_after, 0);
}
