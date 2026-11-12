use super::*;

#[test]
fn completion_survives_reopening_and_pending_updates() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.sqlite3");
    let key = ("1".to_owned(), "/docs/example".to_owned());
    let pending = Record {
        total: 10,
        downloaded: 4,
        completion: None,
    };
    save(
        &db,
        "fixture",
        "one",
        &Records::from([(key.clone(), pending)]),
    )
    .unwrap();
    let completed = Record {
        completion: Some(Completion {
            reason: CompletionReason::RemoteDeleted,
            at: 1,
        }),
        ..pending
    };
    save(
        &db,
        "fixture",
        "one",
        &Records::from([(key.clone(), completed)]),
    )
    .unwrap();
    save(
        &db,
        "fixture",
        "one",
        &Records::from([(key.clone(), pending)]),
    )
    .unwrap();
    assert_eq!(
        load(&db, "fixture", "one").unwrap().get(&key),
        Some(&completed)
    );
    assert!(load(&db, "fixture", "two").unwrap().is_empty());
    assert!(load(&db, "other", "one").unwrap().is_empty());
}
