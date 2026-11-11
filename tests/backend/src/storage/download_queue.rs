use super::*;

#[test]
fn dispositions_survive_reopening_and_are_scoped_to_provider_account_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("queue.sqlite3");
    let key = ("9223372036854775809", "/文件 a");
    save(&db, "baidu", "one", key.0, key.1, Some(Disposition::Paused)).unwrap();
    save(
        &db,
        "baidu",
        "two",
        key.0,
        key.1,
        Some(Disposition::Removed),
    )
    .unwrap();
    assert_eq!(
        load(&db, "baidu", "one").unwrap(),
        HashMap::from([((key.0.to_owned(), key.1.to_owned()), Disposition::Paused)])
    );
    assert!(load(&db, "other", "one").unwrap().is_empty());
    save(&db, "baidu", "one", key.0, key.1, None).unwrap();
    assert!(load(&db, "baidu", "one").unwrap().is_empty());
    assert_eq!(
        load(&db, "baidu", "two").unwrap().into_values().next(),
        Some(Disposition::Removed)
    );
}
