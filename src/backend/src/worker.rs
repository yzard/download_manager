use crate::settings::{BaiduSettings, GeneralSettings, Settings, save_private};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{RwLock, watch},
};

const CLI: &str = "/usr/local/bin/BaiduPCS-Go";
const LOGIN_HASH: &str = "/config/.login-hash";

#[derive(Clone, Serialize)]
pub struct Download {
    pub id: u64,
    pub path: String,
    pub status: &'static str,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed: u64,
}

#[derive(Default)]
pub struct Snapshot {
    pub phase: &'static str,
    files: BTreeMap<u64, Download>,
    metadata_task: Option<u64>,
    queue_ready: bool,
}

impl Snapshot {
    pub fn files(&self) -> Vec<Download> {
        if !self.queue_ready {
            return Vec::new();
        }
        let mut files: Vec<_> = self.files.values().cloned().collect();
        files.sort_by_key(|file| match file.status {
            "active" | "retrying" => 0,
            "queued" => 1,
            _ => 2,
        });
        files
    }

    fn apply_line(&mut self, line: &str) {
        if line.contains("Baidu login failed") || line.contains("未登录") {
            self.phase = "login_failed";
        } else if line.contains("以下文件下载失败") || line.contains("文件或目录不存在")
        {
            self.phase = "download_failed";
        }

        if let Some((id, event)) = task_event(line) {
            self.apply_event(id, event);
            return;
        }

        let mut cells = line
            .split(|c: char| c.is_whitespace() || matches!(c, '|' | '│'))
            .filter(|cell| !cell.is_empty());
        let first_cell = cells.next();
        if first_cell == Some("类型") && cells.next() == Some("目录") {
            if let Some(id) = self.metadata_task {
                self.files.remove(&id);
            }
        } else if first_cell == Some("文件大小")
            && let Some(total) = cells
                .next()
                .and_then(|cell| cell.trim_end_matches(',').parse().ok())
            && let Some(file) = self.metadata_task.and_then(|id| self.files.get_mut(&id))
        {
            file.total = Some(total);
            if total == 0 {
                file.status = "complete";
            }
        }
    }

    fn apply_event(&mut self, id: u64, event: &str) {
        if let Some(path) = event.strip_prefix("加入下载队列: ") {
            self.files.insert(
                id,
                Download {
                    id,
                    path: path.to_owned(),
                    status: "queued",
                    downloaded: 0,
                    total: None,
                    speed: 0,
                },
            );
            return;
        }
        if event == "----" {
            self.metadata_task = Some(id);
            return;
        }
        if event.starts_with("准备下载: ") {
            self.queue_ready = true;
            if self.phase != "download_failed" {
                self.phase = "downloading";
            }
        }
        let Some(file) = self.files.get_mut(&id) else {
            return;
        };
        if event.starts_with("准备下载: ") {
            file.status = "active";
            file.speed = 0;
        } else if let Some(progress) = event.strip_prefix("↓ ") {
            let mut fields = progress.split_whitespace();
            if let Some((done, total)) = fields.next().and_then(|field| field.split_once('/'))
                && let (Some(done), Some(total)) = (human_bytes(done), human_bytes(total))
            {
                file.downloaded = done;
                file.total.get_or_insert(total);
            }
            file.speed = fields
                .next()
                .and_then(|field| field.strip_suffix("/s"))
                .and_then(human_bytes)
                .unwrap_or(0);
        } else if event.starts_with("下载完成, 保存位置:") || event.starts_with("文件已经存在:")
        {
            file.status = if event.starts_with("文件已经存在:") {
                "existing"
            } else {
                "complete"
            };
            file.downloaded = file.total.unwrap_or(file.downloaded);
            file.speed = 0;
        } else if event.contains("重试") {
            file.status = "retrying";
            file.speed = 0;
        } else if event.contains("失败") || event.contains("错误") {
            file.status = "failed";
            file.speed = 0;
            self.phase = "download_failed";
        }
    }
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
            Path::new("/config/pcs_config.json").is_file()
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

async fn download(
    baidu: &BaiduSettings,
    general: &GeneralSettings,
    status: &RwLock<Snapshot>,
) -> io::Result<bool> {
    let destination = general.clone();
    tokio::task::spawn_blocking(move || destination.prepare_destination())
        .await
        .map_err(io::Error::other)??;
    if !login(baidu).await? {
        status.write().await.phase = "login_failed";
        return Ok(false);
    }
    let mut child = Command::new("/bin/busybox")
        .args([
            "sh",
            "-c",
            "exec /usr/local/bin/BaiduPCS-Go \"$@\" 2>&1",
            CLI,
            "download",
            "--saveto",
            &general.download_directory,
            &baidu.monitor_directory,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing CLI output"))?;
    let mut chunk = [0; 8192];
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let count = stdout.read(&mut chunk).await?;
        if count == 0 {
            if !line.is_empty() && !oversized {
                status
                    .write()
                    .await
                    .apply_line(&String::from_utf8_lossy(&line));
            }
            break;
        }
        let mut status = status.write().await;
        for &byte in &chunk[..count] {
            if matches!(byte, b'\r' | b'\n') {
                if !line.is_empty() && !oversized {
                    status.apply_line(&String::from_utf8_lossy(&line));
                }
                line.clear();
                oversized = false;
            } else if line.len() < 65536 {
                line.push(byte);
            } else {
                oversized = true;
            }
        }
    }
    Ok(child.wait().await?.success())
}

pub async fn run(mut settings: watch::Receiver<Settings>, status: &RwLock<Snapshot>) {
    loop {
        let current = settings.borrow_and_update().clone();
        if current.baidu.ready() {
            *status.write().await = Snapshot {
                phase: "checking",
                ..Snapshot::default()
            };
            let result = match download(&current.baidu, &current.general, status).await {
                Ok(completed) => completed,
                Err(error) => {
                    eprintln!("Baidu download check failed: {error}");
                    false
                }
            };
            let mut snapshot = status.write().await;
            snapshot.queue_ready = true;
            for file in snapshot.files.values_mut() {
                if matches!(file.status, "queued" | "active" | "retrying") {
                    file.status = "failed";
                    file.speed = 0;
                }
            }
            if !matches!(snapshot.phase, "login_failed" | "download_failed") {
                snapshot.phase = if result { "idle" } else { "download_failed" };
            }
            eprintln!("Download check finished: {}", snapshot.phase);
        } else {
            *status.write().await = Snapshot {
                phase: "setup",
                ..Snapshot::default()
            };
        }
        tokio::select! {
            changed = settings.changed() => if changed.is_err() { return },
            () = tokio::time::sleep(Duration::from_secs(u64::from(current.baidu.check_interval_seconds))) => {},
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/backend/src/worker.rs"]
mod tests;
