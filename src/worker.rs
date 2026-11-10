use crate::settings::Settings;
use serde::Serialize;
use std::{collections::BTreeMap, io, process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{RwLock, watch},
};

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

async fn download(settings: &Settings, status: &RwLock<Snapshot>) -> io::Result<bool> {
    let mut child = Command::new("/bin/busybox")
        .args([
            "sh",
            "-c",
            "exec /bin/busybox sh /entrypoint.sh auto-download 2>&1",
        ])
        .env("BAIDU_BDUSS", &settings.bduss)
        .env("BAIDU_STOKEN", &settings.stoken)
        .env("MONITOR_DIRECTORY", &settings.monitor_directory)
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

pub async fn run(
    mut settings: watch::Receiver<Settings>,
    status: &RwLock<Snapshot>,
    interval: Duration,
) {
    loop {
        let current = settings.borrow_and_update().clone();
        if current.ready() {
            *status.write().await = Snapshot {
                phase: "checking",
                ..Snapshot::default()
            };
            let result = download(&current, status).await;
            let mut snapshot = status.write().await;
            snapshot.queue_ready = true;
            for file in snapshot.files.values_mut() {
                if matches!(file.status, "queued" | "active" | "retrying") {
                    file.status = "failed";
                    file.speed = 0;
                }
            }
            if !matches!(snapshot.phase, "login_failed" | "download_failed") {
                snapshot.phase = if matches!(result, Ok(true)) {
                    "idle"
                } else {
                    "download_failed"
                };
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
            () = tokio::time::sleep(interval) => {},
        }
    }
}

#[cfg(test)]
mod tests {
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
}
