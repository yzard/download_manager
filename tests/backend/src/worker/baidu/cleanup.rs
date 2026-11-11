use super::*;

#[test]
fn maps_only_exact_descendant_paths() {
    assert_eq!(
        expected_path(
            "/downloads/archive",
            "/work/NAS下载/",
            "/work/NAS下载/sub/file.txt"
        ),
        Some(PathBuf::from("/downloads/archive/NAS下载/sub/file.txt")),
    );
    assert_eq!(
        expected_path("/downloads", "/", "/sub/file.txt"),
        Some(PathBuf::from("/downloads/sub/file.txt")),
    );
    for path in ["/work", "/work-other/a", "/work/../a", "/work/a\\b"] {
        assert!(
            expected_path("/downloads", "/work", path).is_none(),
            "{path}"
        );
    }
}

#[test]
fn rejects_wrong_bytes_resume_files_and_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap().join("fixture.txt");
    let checksum = "b1946ac92492d2347c6235b4d2611184";
    fs::write(&path, b"hello\n").unwrap();
    assert!(verify_local(&path, 6, checksum).is_ok());
    assert!(verify_local(&path, 7, checksum).is_err());
    fs::write(&path, b"world\n").unwrap();
    assert!(verify_local(&path, 6, checksum).is_err());

    fs::write(&path, b"hello\n").unwrap();
    let resume_path = path.with_file_name(format!("fixture.txt{RESUME_SUFFIX}"));
    fs::write(&resume_path, []).unwrap();
    assert!(verify_local(&path, 6, checksum).is_err());
    fs::remove_file(resume_path).unwrap();
    let link = path.with_file_name("link");
    std::os::unix::fs::symlink(path, &link).unwrap();
    assert!(verify_local(&link, 6, checksum).is_err());
}

#[test]
fn distinguishes_missing_metadata_from_transient_or_malformed_output() {
    let path = "/work/file.txt";
    let missing =
        "获取文件/目录的元信息: 遇到错误, 远端服务器返回错误, 代码: 31066, 消息: 文件或目录不存在";
    let other = "获取文件/目录的元信息: 遇到错误, 远端服务器返回错误, 代码: 31034, 消息: temporarily unavailable";
    assert_eq!(
        parse_cloud_file(missing, path).err().unwrap().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        parse_cloud_file(other, path).err().unwrap().to_string(),
        "Provider error code 31034"
    );
    assert_eq!(
        parse_cloud_file("network error", path)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
}
