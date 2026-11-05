use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize)]
pub struct AppPaths {
    pub schema_version: u32,
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub runtime: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "linq", "yuin-cli")
            .context("Cannot determine application directories; check your home directory")?;
        let root = std::env::var_os("YUIN_DATA_DIR").map(PathBuf::from);
        let data = root
            .clone()
            .unwrap_or_else(|| dirs.data_dir().to_path_buf());
        anyhow::ensure!(data.is_absolute(), "YUIN_DATA_DIR must be an absolute path");
        let data = resolve_location(&data)?;
        let digest = Sha256::digest(data.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        #[cfg(unix)]
        let runtime = PathBuf::from(format!(
            "/tmp/yuin-{}-{}",
            unsafe { libc::geteuid() },
            &digest[..16]
        ));
        #[cfg(not(unix))]
        let runtime = data.join("run");
        Ok(Self {
            schema_version: 1,
            config: root.as_ref().map_or_else(
                || dirs.config_dir().join("config.toml"),
                |p| p.join("config.toml"),
            ),
            data,
            cache: root.map_or_else(|| dirs.cache_dir().to_path_buf(), |p| p.join("cache")),
            runtime,
        })
    }

    pub fn socket(&self) -> PathBuf {
        self.runtime.join("daemon.sock")
    }
}

// Aliases for the same data directory must share a daemon lock, including before
// first creation. This function only reads existing ancestors.
fn resolve_location(path: &std::path::Path) -> Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let name = path
                .file_name()
                .context("Application path has an unresolved parent component")?;
            Ok(
                resolve_location(path.parent().context("Application path has no parent")?)?
                    .join(name),
            )
        }
        Err(error) => Err(error.into()),
    }
}
