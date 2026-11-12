use super::*;

#[test]
fn completion_survives_reopening_and_pending_updates() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.sqlite3");
    let key = ("1".to_owned(), "/docs/example".to_owned(), 10);
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

#[test]
fn migrates_v8_history_and_keeps_different_sizes_separate() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.sqlite3");
    let legacy = rusqlite::Connection::open(&db).unwrap();
    legacy
        .execute_batch(include_str!(
            "../../../../src/backend/migrations/008_download_records.sql"
        ))
        .unwrap();
    legacy
        .execute_batch(
            "INSERT INTO download_records
            VALUES ('fixture', 'one', '1', '/docs/example', 10, 10, 'downloaded', 1);
            PRAGMA user_version = 8;",
        )
        .unwrap();
    drop(legacy);

    let pending = Record {
        total: 20,
        downloaded: 0,
        completion: None,
    };
    let key = ("1".to_owned(), "/docs/example".to_owned(), 20);
    save(
        &db,
        "fixture",
        "one",
        &Records::from([(key.clone(), pending)]),
    )
    .unwrap();
    let records = load(&db, "fixture", "one").unwrap();
    assert_eq!(records.get(&key), Some(&pending));
    assert_eq!(
        records.get(&("1".to_owned(), "/docs/example".to_owned(), 10)),
        Some(&Record {
            total: 10,
            downloaded: 10,
            completion: Some(Completion {
                reason: CompletionReason::Downloaded,
                at: 1,
            }),
        })
    );
}
