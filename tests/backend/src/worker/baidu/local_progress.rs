use super::*;

#[test]
fn recovers_saved_ranges_in_both_modes() {
    assert_eq!(
        checkpoint_progress(&[16, 10, 42, 4, 8, 4, 16, 10]),
        Some((4, 10))
    );
    assert_eq!(
        checkpoint_progress(&[8, 1, 16, 10, 24, 6, 42, 4, 8, 4, 16, 6]),
        Some((4, 10))
    );
    assert_eq!(checkpoint_progress(&[16, 10, 42, 4, 8]), None);
    assert_eq!(checkpoint_progress(&[16, 10, 42, 4, 8, 9, 16, 8]), None);
    assert_eq!(
        checkpoint_progress(&[16, 10, 42, 4, 8, 2, 16, 6, 42, 4, 8, 4, 16, 8]),
        None
    );
}

#[test]
fn distinguishes_a_checkpoint_from_an_unverified_local_file() {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().canonicalize().unwrap().join("fixture");
    assert!(read(&file, 10).is_none());
    fs::write(&file, b"1234").unwrap();
    assert_eq!(
        read(&file, 10),
        Some(LocalProgress {
            bytes: 4,
            total: 10,
            kind: LocalProgressKind::LocalFile
        })
    );
    let marker = file.with_file_name(format!("fixture{RESUME_SUFFIX}"));
    fs::write(&marker, [16, 10, 42, 4, 8, 4, 16, 10]).unwrap();
    assert_eq!(
        read(&file, 10),
        Some(LocalProgress {
            bytes: 4,
            total: 10,
            kind: LocalProgressKind::Checkpoint
        })
    );
    assert!(read(&file, 99).is_none());
    fs::write(&marker, [16, 10, 42, 4, 8]).unwrap();
    assert!(read(&file, 10).is_none());
}
