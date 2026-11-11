mod cleanup;
mod listing;

use super::{
    Download, DownloadProvider, DownloadSession, DownloadState, DownloadTransfer, FailureReason,
    RemoteFile, TransferEvent, TransferProgress,
};
use crate::settings::{BaiduSettings, Settings, save_private};
use cleanup::{CloudFile, FileMetadata};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs, io, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdout, Command},
    sync::watch,
    time::timeout,
};

const CLI: &str = "/usr/local/bin/BaiduPCS-Go";
const PCS_CONFIG: &str = "/config/pcs_config.json";
const LOGIN_HASH: &str = "/config/.login-hash";

pub(super) async fn run_cli(args: &[&str]) -> io::Result<String> {
    let output = timeout(
        Duration::from_secs(30),
        Command::new(CLI)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    if !output.status.success() {
        return Err(io::Error::other("BaiduPCS-Go command failed"));
    }
    String::from_utf8(output.stdout).map_err(io::Error::other)
}

async fn active_account() -> io::Result<u64> {
    #[derive(Deserialize)]
    struct ActiveAccount {
        baidu_active_uid: u64,
    }
    tokio::task::spawn_blocking(|| {
        let contents = fs::read(PCS_CONFIG)?;
        let config: ActiveAccount = serde_json::from_slice(&contents).map_err(io::Error::other)?;
        if config.baidu_active_uid == 0 {
            return Err(io::Error::other("Baidu account is not active"));
        }
        Ok(config.baidu_active_uid)
    })
    .await
    .map_err(io::Error::other)?
}

fn task_event(line: &str) -> Option<(u64, &str)> {
    let (id, event) = line.strip_prefix('[')?.split_once("] ")?;
    Some((id.parse().ok()?, event))
}

fn human_bytes(value: &str) -> Option<u64> {
    let split = value.find(|c: char| c.is_ascii_alphabetic())?;
    let number: f64 = value[..split].parse().ok()?;
    let power = match &value[split..] {
        "B" => 0,
        "KB" => 1,
        "MB" => 2,
        "GB" => 3,
        "TB" => 4,
        "PB" => 5,
        _ => return None,
    };
    Some((number * 1024_f64.powi(power)).round() as u64)
}

async fn login(settings: &BaiduSettings) -> io::Result<bool> {
    let mut hash = Sha256::new();
    hash.update(settings.bduss.as_bytes());
    hash.update(b"\n");
    hash.update(settings.stoken.as_bytes());
    let fingerprint = format!("{:x}", hash.finalize());
    let cached = tokio::task::spawn_blocking({
        let fingerprint = fingerprint.clone();
        move || {
            Path::new(PCS_CONFIG).is_file()
                && fs::read_to_string(LOGIN_HASH).is_ok_and(|saved| saved.trim_end() == fingerprint)
        }
    })
    .await
    .map_err(io::Error::other)?;
    if cached {
        return Ok(true);
    }
    let output = Command::new(CLI)
        .arg("login")
        .arg(format!("-bduss={}", settings.bduss))
        .arg(format!("-stoken={}", settings.stoken))
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout).contains("百度帐号登录成功:")
    {
        return Ok(false);
    }
    tokio::task::spawn_blocking(move || {
        save_private(Path::new(LOGIN_HASH), fingerprint.as_bytes())
    })
    .await
    .map_err(io::Error::other)??;
    Ok(true)
}

pub struct Baidu;

pub struct BaiduSession {
    current: Settings,
    account: String,
}

impl DownloadProvider for Baidu {
    const ID: &'static str = "baidu";
    type Session = BaiduSession;

    fn ready(&self, settings: &Settings) -> bool {
        settings.baidu.ready()
    }

    fn scan_interval(&self, settings: &Settings) -> Duration {
        Duration::from_secs(u64::from(settings.baidu.check_interval_seconds))
    }

    fn recycle_on_remove(settings: &Settings) -> bool {
        settings.baidu.delete_on_remove
    }

    async fn connect(&self, settings: &Settings) -> io::Result<Self::Session> {
        if !timeout(Duration::from_secs(60), login(&settings.baidu)).await?? {
            return Err(io::Error::other("Baidu login failed"));
        }
        Ok(BaiduSession {
            current: settings.clone(),
            account: active_account().await?.to_string(),
        })
    }
}

impl BaiduSession {
    fn local_path(&self, path: &str) -> Option<std::path::PathBuf> {
        cleanup::expected_path(
            &self.current.general.download_directory,
            &self.current.baidu.monitor_directory,
            path,
        )
    }
}

impl DownloadSession for BaiduSession {
    type Transfer = BaiduTransfer;

    fn account(&self) -> &str {
        &self.account
    }

    fn current(&self, latest: &Settings) -> bool {
        latest.baidu.bduss == self.current.baidu.bduss
            && latest.baidu.stoken == self.current.baidu.stoken
            && latest.baidu.monitor_directory == self.current.baidu.monitor_directory
            && latest.general.download_directory == self.current.general.download_directory
    }

    fn contains(&self, path: &str) -> bool {
        self.local_path(path).is_some()
    }

    async fn list(&self) -> io::Result<Vec<RemoteFile>> {
        listing::discover(&self.current.baidu.monitor_directory).await
    }

    async fn start(&self, file: &Download) -> io::Result<BaiduTransfer> {
        let output = self
            .local_path(&file.path)
            .ok_or_else(|| io::Error::other("Invalid Baidu download path"))?;
        let destination = output
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| io::Error::other("Invalid Baidu download destination"))?
            .to_owned();
        let local = crate::settings::GeneralSettings {
            download_directory: destination.clone(),
            ..self.current.general.clone()
        };
        tokio::task::spawn_blocking(move || {
            local.prepare_destination()?;
            let mut marker = output.as_os_str().to_owned();
            marker.push(cleanup::RESUME_SUFFIX);
            for path in [output.as_os_str(), marker.as_os_str()] {
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.is_file() => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Ok(_) => return Err(io::Error::other("Download path is not a regular file")),
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })
        .await
        .map_err(io::Error::other)??;
        let literal = listing::literal_path(&file.path)
            .ok_or_else(|| io::Error::other("Invalid Baidu file path"))?;
        let cloud = match cleanup::cloud_file(&file.path).await {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::ConnectionAborted
                ) =>
            {
                tokio::time::sleep(Duration::from_secs(1)).await;
                cleanup::cloud_file(&file.path).await?
            }
            result => result?,
        };
        if cloud.fs_id.to_string() != file.remote_id {
            return Err(io::Error::other(FailureReason::FileChanged));
        }
        let mut child = Command::new("/bin/busybox")
            .args([
                "sh",
                "-c",
                "exec /usr/local/bin/BaiduPCS-Go \"$@\" 2>&1",
                CLI,
                "download",
                "--saveto",
                &destination,
                &literal,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("Missing CLI output"))?;
        let mut progress = BaiduProgress::new(file, cloud.fs_id);
        progress.value.total = Some(cloud.size);
        Ok(BaiduTransfer {
            child,
            stdout: BufReader::new(stdout),
            progress,
            line: Vec::new(),
            oversized: false,
        })
    }

    async fn recycle(
        &self,
        file: &Download,
        settings: &watch::Receiver<Settings>,
    ) -> io::Result<()> {
        if !self.contains(&file.path) {
            return Err(io::Error::other(
                "Baidu file is outside the monitored folder",
            ));
        }
        let cloud = match cleanup::cloud_file(&file.path).await {
            Ok(cloud) => cloud,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if cloud.fs_id.to_string() != file.remote_id
            || active_account().await?.to_string() != self.account
            || !self.current(&settings.borrow())
            || !Baidu::recycle_on_remove(&settings.borrow())
        {
            return Err(io::Error::other(
                "Baidu file or configuration changed before removal",
            ));
        }
        cleanup::move_to_recycle_bin(&file.path).await
    }

    async fn after_complete(
        &self,
        transfer: &BaiduTransfer,
        settings: &watch::Receiver<Settings>,
    ) -> io::Result<bool> {
        if !self.current.baidu.delete_after_download || transfer.progress.saved_path.is_none() {
            return Ok(false);
        }
        cleanup::recycle_file(transfer, &self.current, settings).await
    }
}

struct BaiduProgress {
    path: String,
    file_id: u64,
    task: Option<u64>,
    metadata: FileMetadata,
    finished: bool,
    failed: bool,
    failure: Option<FailureReason>,
    cloud_file: Option<CloudFile>,
    saved_path: Option<String>,
    value: TransferProgress,
}

impl BaiduProgress {
    fn new(file: &Download, file_id: u64) -> Self {
        Self {
            path: file.path.clone(),
            file_id,
            task: None,
            metadata: FileMetadata::default(),
            finished: false,
            failed: false,
            failure: None,
            cloud_file: None,
            saved_path: None,
            value: file.progress,
        }
    }

    fn apply(&mut self, line: &str) {
        if line.starts_with("下载结束, 时间:") {
            self.finished = true;
        } else if line.starts_with("以下文件下载失败") {
            self.failed = true;
        } else if self.task.is_none() && line.starts_with("未登录") {
            self.failed = true;
            self.failure = Some(FailureReason::LoginRequired);
        }
        if let Some((id, event)) = task_event(line) {
            if let Some(path) = event.strip_prefix("加入下载队列: ") {
                if path == self.path {
                    self.task = Some(id);
                }
                return;
            }
            if self.task == Some(id) {
                self.apply_event(event);
            }
        } else if self.task.is_some()
            && let Some(cloud) = self.metadata.read_line(line)
            && cloud.path == self.path
            && cloud.fs_id == self.file_id
        {
            self.value.total = Some(cloud.size);
            if cloud.size == 0 {
                self.value.status = DownloadState::Complete;
            }
            self.cloud_file = Some(cloud);
        }
    }

    fn apply_event(&mut self, event: &str) {
        let value = &mut self.value;
        if event == "----" {
            self.failure = None;
            self.metadata = FileMetadata::default();
            self.cloud_file = None;
            self.saved_path = None;
        } else if let Some(path) = event.strip_prefix("准备下载: ") {
            value.status = DownloadState::Active;
            value.speed = 0;
            self.saved_path = None;
            if self.path != path {
                self.cloud_file = None;
            }
        } else if let Some(progress) = event.strip_prefix("↓ ") {
            let mut fields = progress.split_whitespace();
            if let Some((done, total)) = fields.next().and_then(|field| field.split_once('/'))
                && let (Some(done), Some(total)) = (human_bytes(done), human_bytes(total))
            {
                value.downloaded = done;
                value.total.get_or_insert(total);
            }
            value.speed = fields
                .next()
                .and_then(|field| field.strip_suffix("/s"))
                .and_then(human_bytes)
                .unwrap_or(0);
        } else if let Some(path) = event.strip_prefix("下载完成, 保存位置: ") {
            if value.status == DownloadState::Active {
                self.saved_path = Some(path.to_owned());
            }
            value.status = DownloadState::Complete;
            value.downloaded = value.total.unwrap_or(value.downloaded);
            value.speed = 0;
        } else if event.starts_with("文件已经存在:") {
            value.status = DownloadState::Existing;
            value.downloaded = value.total.unwrap_or(value.downloaded);
            value.speed = 0;
        } else if event.contains("重试") {
            self.failure = transfer_failure(event).or(self.failure);
            value.status = DownloadState::Retrying;
            value.speed = 0;
            self.saved_path = None;
        } else if event.contains("失败") || event.contains("错误") {
            self.failure = transfer_failure(event).or(self.failure);
            value.status = DownloadState::Failed;
            value.speed = 0;
            self.saved_path = None;
        }
    }
}

fn transfer_failure(event: &str) -> Option<FailureReason> {
    if let Some(code) = cleanup::pcs_error_code(event) {
        return Some(FailureReason::ProviderError(code));
    }
    if event.contains("TLS handshake timeout") {
        Some(FailureReason::TlsHandshakeTimeout)
    } else if event.contains("timeout") || event.contains("超时") {
        Some(FailureReason::Timeout)
    } else if event.contains("网络错误") {
        Some(FailureReason::Network)
    } else if event.contains("校验失败") || event.contains("md5值与服务器记录的不匹配")
    {
        Some(FailureReason::ChecksumMismatch)
    } else {
        None
    }
}

pub struct BaiduTransfer {
    child: Child,
    stdout: BufReader<ChildStdout>,
    progress: BaiduProgress,
    line: Vec<u8>,
    oversized: bool,
}

impl DownloadTransfer for BaiduTransfer {
    async fn next(&mut self) -> io::Result<TransferEvent> {
        let bytes = self.stdout.fill_buf().await?;
        let count = bytes.len();
        for &byte in bytes {
            if matches!(byte, b'\r' | b'\n') {
                if !self.line.is_empty() && !self.oversized {
                    self.progress.apply(&String::from_utf8_lossy(&self.line));
                }
                self.line.clear();
                self.oversized = false;
            } else if self.line.len() < 65536 {
                self.line.push(byte);
            } else {
                self.oversized = true;
            }
        }
        self.stdout.consume(count);
        if count != 0 {
            // The CLI reports a file as complete before its own verification.
            let mut value = self.progress.value;
            if matches!(
                value.status,
                DownloadState::Complete | DownloadState::Existing | DownloadState::Failed
            ) {
                value.status = DownloadState::Active;
            }
            return Ok(TransferEvent::Progress(value));
        }
        if !self.line.is_empty() && !self.oversized {
            self.progress.apply(&String::from_utf8_lossy(&self.line));
            self.line.clear();
        }
        let mut value = self.progress.value;
        let exit = self.child.wait().await?;
        let failure = if !exit.success()
            || !self.progress.finished
            || self.progress.failed
            || !matches!(
                value.status,
                DownloadState::Complete | DownloadState::Existing
            ) {
            value.status = DownloadState::Failed;
            let reason = self.progress.failure.unwrap_or(FailureReason::NoCompletion);
            eprintln!(
                "Baidu transfer failed for {:?}: {reason} ({exit})",
                self.progress.path
            );
            Some(reason)
        } else {
            None
        };
        value.speed = 0;
        Ok(TransferEvent::Finished(value, failure))
    }

    async fn stop(&mut self) -> io::Result<()> {
        self.child.kill().await
    }
}

#[cfg(test)]
#[path = "../../../../../tests/backend/src/worker/baidu.rs"]
mod tests;
