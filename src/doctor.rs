use std::{env, path::PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::paths::AppPaths;

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    version: &'static str,
    phase: &'static str,
    playback_implemented: bool,
    mpv: Option<PathBuf>,
    paths: AppPaths,
}

pub fn run(json: bool) -> Result<()> {
    let program = PathBuf::from(
        env::var_os("YUIN_MPV")
            .unwrap_or_else(|| if cfg!(windows) { "mpv.exe" } else { "mpv" }.into()),
    );
    let mpv = if program.components().count() > 1 || program.is_absolute() {
        is_executable(&program).then_some(program)
    } else {
        env::var_os("PATH").and_then(|value| {
            env::split_paths(&value)
                .map(|directory| directory.join(&program))
                .find(|candidate| is_executable(candidate))
        })
    };
    let report = Report {
        schema_version: 1,
        version: env!("CARGO_PKG_VERSION"),
        phase: "s4-integration",
        playback_implemented: cfg!(unix),
        mpv,
        paths: AppPaths::discover()?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Yuin CLI {} — music player", report.version);
        match report.mpv {
            Some(binary) => println!("mpv: {}", binary.display()),
            None => println!("mpv: missing (install it to play local audio)"),
        }
        println!("Config: {}", report.paths.config.display());
        println!("Data:   {}", report.paths.data.display());
        println!("Cache:  {}", report.paths.cache.display());
        println!("Runtime: {}", report.paths.runtime.display());
        println!(
            "Catalog, LX source, local/online playback and validated downloads are available."
        );
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
