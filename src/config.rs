use crate::{domain::Platform, paths::AppPaths};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub platform: Platform,
    pub source: Option<Uuid>,
    pub play_quality: String,
    pub download_quality: String,
    pub download_dir: PathBuf,
    pub download_concurrency: usize,
    pub language: String,
    pub theme: String,
    pub fade_seconds: f64,
    pub media_keys: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            platform: Platform::Wy,
            source: None,
            play_quality: "320k".into(),
            download_quality: "320k".into(),
            download_dir: PathBuf::new(),
            download_concurrency: 2,
            language: "zh-CN".into(),
            theme: "macchiato".into(),
            fade_seconds: 0.0,
            media_keys: false,
        }
    }
}

pub const QUALITIES: [&str; 4] = ["128k", "320k", "flac", "flac24bit"];

impl Config {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        let mut value = if paths.config.exists() {
            let bytes = std::fs::read(&paths.config)?;
            ensure!(bytes.len() <= 64 * 1024, "Configuration exceeds 64 KiB");
            toml::from_str(std::str::from_utf8(&bytes)?).context("Invalid config.toml")?
        } else {
            Self::default()
        };
        if value.download_dir.as_os_str().is_empty() {
            value.download_dir = paths.data.join("downloads");
        }
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            QUALITIES.contains(&self.play_quality.as_str())
                && QUALITIES.contains(&self.download_quality.as_str()),
            "Unsupported quality"
        );
        ensure!(
            (1..=2).contains(&self.download_concurrency),
            "download_concurrency must be 1 or 2"
        );
        ensure!(
            ["zh-CN", "en"].contains(&self.language.as_str()),
            "language must be zh-CN or en"
        );
        ensure!(
            ["macchiato", "terminal", "dark", "light"].contains(&self.theme.as_str()),
            "theme must be macchiato, terminal, dark or light"
        );
        ensure!(
            self.download_dir.is_absolute(),
            "download_dir must be absolute"
        );
        ensure!(
            self.fade_seconds.is_finite() && (0.0..=10.0).contains(&self.fade_seconds),
            "fade_seconds must be between 0 and 10"
        );
        Ok(())
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "platform" => self.platform = value.parse()?,
            "play_quality" => self.play_quality = value.into(),
            "download_quality" => self.download_quality = value.into(),
            "download_dir" => self.download_dir = value.into(),
            "download_concurrency" => self.download_concurrency = value.parse()?,
            "language" => self.language = value.into(),
            "theme" => self.theme = value.into(),
            "fade_seconds" => self.fade_seconds = value.parse()?,
            "media_keys" => self.media_keys = value.parse()?,
            _ => anyhow::bail!("Unknown configuration key: {key}"),
        }
        self.validate()
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("File needs a parent directory")?;
    crate::secure::create_directory(parent)?;
    let temp = parent.join(format!(".yuin-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = crate::secure::private_file(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
