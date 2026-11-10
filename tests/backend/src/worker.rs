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
