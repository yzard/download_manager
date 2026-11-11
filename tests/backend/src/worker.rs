use super::*;

#[test]
fn consumes_directory_file_queue_progress_and_completion() {
    let mut s = Snapshot::default();
    for line in [
        "[1] 加入下载队列: /NAS下载",
        "[2] 加入下载队列: /NAS下载/first video.mp4",
        "[3] 加入下载队列: /NAS下载/second.txt",
        "[1] ----",
        "│ 类型 │ 目录 │",
        "[2] ----",
        "│ 文件大小 │ 230133000, 219.47MB │",
        "[2] 准备下载: /NAS下载/first video.mp4",
        "[2] ↓ 8.00MB/219.47MB 120.5KB/s in 2m10s, left - ............",
    ] {
        s.apply_line(line);
    }
    let files = s.files();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].status, "active");
    assert_eq!(files[0].total, Some(230133000));
    assert_eq!(files[0].downloaded, 8388608);
    assert_eq!(files[0].speed, 123392);
    assert_eq!(files[1].status, "queued");
    s.apply_line("[2] 下载完成, 保存位置: /downloads/first video.mp4");
    assert_eq!(s.files()[1].status, "complete");
}

#[test]
fn tracks_new_downloads_with_matching_cloud_metadata() {
    let mut snapshot = Snapshot::default();
    for line in [
        "[1] 加入下载队列: /NAS下载/新文件.txt",
        "[2] 加入下载队列: /NAS下载/已有.txt",
        "[1] ----",
        "   类型                  文件",
        "   文件路径              /NAS下载/新文件.txt",
        "   文件大小              6, 6B",
        "   md5 (截图请打码)       b1946ac92492d2347c6235b4d2611184",
        "   fs_id                 123",
        "[1] 准备下载: /NAS下载/新文件.txt",
        "[1] 下载完成, 保存位置: /downloads/NAS下载/新文件.txt",
        "[2] 准备下载: /NAS下载/已有.txt",
        "[2] 文件已经存在: /downloads/NAS下载/已有.txt, 跳过...",
        "下载结束, 时间: 1s, 数据总量: 6B",
    ] {
        snapshot.apply_line(line);
    }
    assert!(snapshot.round_finished);
    let file = &snapshot.files[&1];
    assert_eq!(file.cloud_file.as_ref().unwrap().path, file.path);
    assert_eq!(
        file.saved_path.as_deref(),
        Some("/downloads/NAS下载/新文件.txt")
    );
    assert!(snapshot.files[&2].saved_path.is_none());

    snapshot.apply_line("[1] 文件校验失败");
    assert!(snapshot.files[&1].saved_path.is_none());
}
