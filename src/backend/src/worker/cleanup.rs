use super::{CLI, Download};
use crate::settings::Settings;
use md5::{Digest, Md5};
use std::{
    fs::{self, File, Metadata},
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{process::Command, sync::watch, time::timeout};

const RESUME_SUFFIX: &str = ".BaiduPCS-Go-downloading";
const RECYCLE_SUCCESS: &str = "操作成功, 以下文件/目录已删除, 可在网盘文件回收站找回:";

#[derive(Clone, PartialEq, Eq)]
pub(super) struct CloudFile {
    pub(super) path: String,
    size: u64,
    md5: String,
    fs_id: u64,
}

#[derive(Default)]
pub(super) struct FileMetadata {
    is_file: bool,
    path: Option<String>,
    size: Option<u64>,
    md5: Option<String>,
}

impl FileMetadata {
    pub(super) fn read_line(&mut self, line: &str) -> Option<CloudFile> {
        // BaiduPCS-Go v4.0.2 prints a table with padded labels and no borders.
        let line = line.trim_start();
        if let Some(kind) = table_value(line, "类型") {
            *self = Self {
                is_file: kind == "文件",
                ..Self::default()
            };
        } else if self.is_file {
            if let Some(path) = table_value(line, "文件路径") {
                self.path = Some(path.to_owned());
            } else if let Some(size) = table_value(line, "文件大小") {
                self.size = size
                    .split_once(',')
                    .and_then(|(bytes, _)| bytes.parse().ok());
            } else if let Some(md5) = table_value(line, "md5 (截图请打码)")
                .or_else(|| table_value(line, "md5 (可能不正确)"))
            {
                self.md5 = (md5.len() == 32 && md5.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then(|| md5.to_ascii_lowercase());
            } else if let Some(fs_id) = table_value(line, "fs_id") {
                let file = std::mem::take(self);
                return Some(CloudFile {
                    path: file.path?,
                    size: file.size?,
                    md5: file.md5?,
                    fs_id: fs_id.parse::<u64>().ok().filter(|id| *id != 0)?,
                });
            }
        }
        None
    }
}

fn table_value<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let value = line.strip_prefix(label)?;
    value.starts_with(char::is_whitespace).then(|| value.trim())
}

fn expected_path(destination: &str, monitor: &str, remote: &str) -> Option<PathBuf> {
    if remote.contains(['*', '?', '[', ']', '\\'])
        || remote.chars().any(char::is_control)
        || !remote
            .strip_prefix('/')?
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
    {
        return None;
    }
    let monitor = Path::new(monitor);
    let relative = Path::new(remote).strip_prefix(monitor).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    let mut path: PathBuf = Path::new(destination).components().collect();
    if let Some(name) = monitor.file_name() {
        path.push(name);
    }
    Some(path.join(relative))
}

fn file_metadata(path: &Path) -> io::Result<Metadata> {
    if !path.is_absolute() || fs::canonicalize(path)? != path {
        return Err(io::Error::other("Download path contains a symbolic link"));
    }
    let mut resume_path = path.as_os_str().to_owned();
    resume_path.push(RESUME_SUFFIX);
    match fs::symlink_metadata(resume_path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(_) => return Err(io::Error::other("Download resume file still exists")),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::other("Download path is not a regular file"));
    }
    Ok(metadata)
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

fn verify_local(path: &Path, size: u64, expected_md5: &str) -> io::Result<Metadata> {
    let before = file_metadata(path)?;
    if before.len() != size {
        return Err(io::Error::other(
            "Local file size differs from the Baidu file size",
        ));
    }
    let mut file = File::open(path)?;
    if !same_file(&before, &file.metadata()?) {
        return Err(io::Error::other("Local file changed before verification"));
    }
    let mut md5 = Md5::new();
    io::copy(&mut file, &mut md5)?;
    if format!("{:x}", md5.finalize()) != expected_md5 {
        return Err(io::Error::other(
            "Local file checksum differs from the Baidu checksum",
        ));
    }
    file.sync_all()?;
    File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("Missing download directory"))?,
    )?
    .sync_all()?;
    if !same_file(&before, &file_metadata(path)?) {
        return Err(io::Error::other("Local file changed during verification"));
    }
    Ok(before)
}

async fn run_cli(action: &str, path: &str) -> io::Result<Output> {
    timeout(
        Duration::from_secs(30),
        Command::new(CLI)
            .args([action, path])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await?
}

fn cleanup_enabled(current: &Settings, latest: &Settings) -> bool {
    latest.baidu.delete_after_download
        && latest.baidu.bduss == current.baidu.bduss
        && latest.baidu.stoken == current.baidu.stoken
        && latest.baidu.monitor_directory == current.baidu.monitor_directory
        && latest.general.download_directory == current.general.download_directory
}

async fn recycle_file(
    file: Download,
    current: &Settings,
    settings: &watch::Receiver<Settings>,
) -> io::Result<bool> {
    let cloud = file
        .cloud_file
        .ok_or_else(|| io::Error::other("Missing Baidu file metadata"))?;
    let path = expected_path(
        &current.general.download_directory,
        &current.baidu.monitor_directory,
        &cloud.path,
    )
    .filter(|path| path.to_str() == file.saved_path.as_deref())
    .ok_or_else(|| io::Error::other("Download path does not match the Baidu file path"))?;
    let before = tokio::task::spawn_blocking({
        let path = path.clone();
        let md5 = cloud.md5.clone();
        let size = cloud.size;
        move || verify_local(&path, size, &md5)
    })
    .await
    .map_err(io::Error::other)??;

    let output = run_cli("meta", &cloud.path).await?;
    let mut metadata = FileMetadata::default();
    let mut files = std::str::from_utf8(&output.stdout)
        .map_err(io::Error::other)?
        .lines()
        .filter_map(|line| metadata.read_line(line));
    if !output.status.success() || files.next().as_ref() != Some(&cloud) || files.next().is_some() {
        return Err(io::Error::other(
            "Baidu file changed or its identity could not be verified",
        ));
    }
    let after = tokio::task::spawn_blocking(move || file_metadata(&path))
        .await
        .map_err(io::Error::other)??;
    if !same_file(&before, &after) {
        return Err(io::Error::other("Local file changed before recycling"));
    }
    if !cleanup_enabled(current, &settings.borrow()) {
        return Ok(false);
    }
    let output = run_cli("rm", &cloud.path).await?;
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim_end() == RECYCLE_SUCCESS)
    {
        return Err(io::Error::other(
            "Baidu did not confirm that the file moved to the recycle bin",
        ));
    }
    Ok(true)
}

pub(super) async fn recycle_completed(
    candidates: Vec<Download>,
    current: &Settings,
    settings: &watch::Receiver<Settings>,
) {
    for file in candidates {
        if !cleanup_enabled(current, &settings.borrow()) {
            return;
        }
        let path = file.path.clone();
        match recycle_file(file, current, settings).await {
            Ok(true) => eprintln!("Baidu moved {path:?} to the recycle bin"),
            Ok(false) => return,
            Err(error) => eprintln!("Baidu recycle request did not complete for {path:?}: {error}"),
        }
    }
}

#[cfg(test)]
#[path = "../../../../tests/backend/src/worker/cleanup.rs"]
mod tests;
