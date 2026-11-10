use std::{fs, io, io::Write, path::Path};

pub const DEFAULT_CHECK_INTERVAL_SECONDS: u32 = 300;

#[derive(Clone)]
pub struct BaiduSettings {
    pub bduss: String,
    pub stoken: String,
    pub monitor_directory: String,
    pub download_directory: String,
    pub check_interval_seconds: u32,
}

impl Default for BaiduSettings {
    fn default() -> Self {
        Self {
            bduss: String::new(),
            stoken: String::new(),
            monitor_directory: String::new(),
            download_directory: "/downloads".to_owned(),
            check_interval_seconds: DEFAULT_CHECK_INTERVAL_SECONDS,
        }
    }
}

impl BaiduSettings {
    pub fn has_tokens(&self) -> bool {
        !self.bduss.trim().is_empty() && !self.stoken.trim().is_empty()
    }

    pub fn ready(&self) -> bool {
        self.validate().is_ok()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.has_tokens()
            || [&self.bduss, &self.stoken].iter().any(|token| {
                token.len() > 4096 || token.chars().any(|c| c.is_whitespace() || c.is_control())
            })
        {
            return Err("请填写有效的 BDUSS 和 STOKEN。");
        }
        if !self.monitor_directory.starts_with('/')
            || self.monitor_directory.len() > 4096
            || self.monitor_directory.chars().any(char::is_control)
            || self
                .monitor_directory
                .split('/')
                .any(|part| matches!(part, "." | ".."))
            || self.monitor_directory.contains(['*', '?', '[', ']', '\\'])
        {
            return Err("请填写百度网盘里的绝对目录，例如 /NAS下载。目录不能包含通配符。");
        }
        if !Path::new(&self.download_directory).starts_with("/downloads")
            || self.download_directory.len() > 4096
            || self.download_directory.chars().any(char::is_control)
            || self
                .download_directory
                .split('/')
                .any(|part| matches!(part, "." | ".."))
        {
            return Err("保存位置必须是 /downloads 或其下的子目录。");
        }
        if self.check_interval_seconds == 0 {
            return Err("检查间隔至少为 1 秒。");
        }
        Ok(())
    }

    pub fn prepare_destination(&self) -> io::Result<()> {
        let mut path = Path::new("/downloads").to_path_buf();
        let relative = Path::new(&self.download_directory)
            .strip_prefix(&path)
            .map_err(io::Error::other)?;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(io::Error::other("Invalid download directory"));
            };
            path.push(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => {
                    return Err(io::Error::other(
                        "Download directory is not a real directory",
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&path)?,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

pub fn save_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing configuration directory"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
