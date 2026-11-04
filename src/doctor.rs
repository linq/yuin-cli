use std::{env, path::PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::paths::AppPaths;

#[derive(Serialize)]
struct Report {
    version: &'static str,
    phase: &'static str,
    playback_implemented: bool,
    mpv: Option<PathBuf>,
    paths: AppPaths,
}

pub fn run(json: bool) -> Result<()> {
    let mpv = env::var_os("PATH").and_then(|value| {
        env::split_paths(&value)
            .map(|directory| directory.join(if cfg!(windows) { "mpv.exe" } else { "mpv" }))
            .find(|candidate| is_executable(candidate))
    });
    let report = Report {
        version: env!("CARGO_PKG_VERSION"),
        phase: "scaffold",
        playback_implemented: false,
        mpv,
        paths: AppPaths::discover()?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Yuin CLI {} — project scaffold", report.version);
        match report.mpv {
            Some(binary) => println!("mpv: {}", binary.display()),
            None => println!("mpv: missing (needed for the planned playback backend)"),
        }
        println!("Config: {}", report.paths.config.display());
        println!("Data:   {}", report.paths.data.display());
        println!("Cache:  {}", report.paths.cache.display());
        println!("Catalog, source execution, playback, and downloads are not implemented yet.");
    }
    Ok(())
}

fn is_executable(candidate: &std::path::Path) -> bool {
    let Ok(metadata) = candidate.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
