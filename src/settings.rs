use crate::auth::Account;
use serde::{Deserialize, Serialize};
use std::{env, fs, io, io::Write, path::Path};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub bduss: String,
    pub stoken: String,
    pub monitor_directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, &'static str> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let settings: Self =
                    toml::from_str(&text).map_err(|_| "Invalid /config/config.toml")?;
                Ok(settings)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self {
                bduss: env::var("BAIDU_BDUSS").unwrap_or_default(),
                stoken: env::var("BAIDU_STOKEN").unwrap_or_default(),
                monitor_directory: env::var("MONITOR_DIRECTORY").unwrap_or_default(),
                account: None,
            }),
            Err(_) => Err("Cannot read /config/config.toml"),
        }
    }

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
        Ok(())
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string(self).map_err(io::Error::other)?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Missing configuration directory"))?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(text.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}
