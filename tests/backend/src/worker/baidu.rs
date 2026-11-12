use super::*;

#[test]
fn parses_progress_and_rejects_metadata_for_a_replaced_file() {
    let file = Download {
        id: "fixture".to_owned(),
        provider: "baidu",
        path: "/NAS下载/新文件.txt".to_owned(),
        remote_id: "123".to_owned(),
        last_failure: None,
        local_progress: None,
        completion: None,
        progress: TransferProgress {
            status: DownloadState::Active,
            downloaded: 0,
            total: Some(230_000_000),
            speed: 0,
        },
    };
    let mut progress = BaiduProgress::new(&file, 123);
    for line in [
        "[1] 加入下载队列: /NAS下载/新文件.txt",
        "[1] ----",
        "   类型                  文件",
        "   文件路径              /NAS下载/新文件.txt",
        "   文件大小              230133000, 219.47MB",
        "   md5 (截图请打码)       b1946ac92492d2347c6235b4d2611184",
        "   fs_id                 456",
        "[1] 准备下载: /NAS下载/新文件.txt",
        "[1] ↓ 8.00MB/219.47MB 120.5KB/s in 2m10s, left - ............",
    ] {
        progress.apply(line);
    }
    assert!(progress.cloud_file.is_none());
    assert_eq!(progress.value.downloaded, 8388608);
    assert_eq!(progress.value.speed, 123392);
    progress.apply("[1] 下载完成, 保存位置: /downloads/NAS下载/新文件.txt");
    progress.apply("下载结束, 时间: 1s, 数据总量: 219.47MB");
    assert!(progress.finished);
    assert_eq!(progress.value.status, DownloadState::Complete);
    assert!(progress.saved_path.is_some());
}

#[test]
fn reports_only_a_classified_reason_from_provider_output() {
    assert_eq!(
        transfer_failure(
            "下载失败: 网络错误, TLS handshake timeout https://example.test/sensitive"
        ),
        Some(FailureReason::TlsHandshakeTimeout)
    );
    assert_eq!(
        transfer_failure("遇到错误, 远端服务器返回错误, 代码: 31034, 消息: internal"),
        Some(FailureReason::ProviderError(31034))
    );
    assert_eq!(transfer_failure("下载失败: unrecognized detail"), None);
}
