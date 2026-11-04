use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Serialize;

#[derive(Serialize)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "linq", "yuin-cli")
            .context("Cannot determine application directories; check your home directory")?;
        Ok(Self {
            config: dirs.config_dir().join("config.toml"),
            data: dirs.data_dir().to_path_buf(),
            cache: dirs.cache_dir().to_path_buf(),
        })
    }
}
