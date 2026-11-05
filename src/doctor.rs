use crate::paths::AppPaths;
use anyhow::Result;
use serde::Serialize;

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    version: &'static str,
    phase: &'static str,
    playback_implemented: bool,
    audio_backend: &'static str,
    external_player_required: bool,
    paths: AppPaths,
}
pub fn run(json: bool) -> Result<()> {
    // Diagnostics remain read-only and never open an audio device or start a daemon.
    let report = Report {
        schema_version: 1,
        version: env!("CARGO_PKG_VERSION"),
        phase: "s4-integration",
        playback_implemented: cfg!(unix),
        audio_backend: "symphonia+rodio",
        external_player_required: false,
        paths: AppPaths::discover()?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Yuin CLI {} — music player", report.version);
        println!("Audio: built-in Symphonia decoder and system audio output (no mpv required)");
        println!("Config: {}", report.paths.config.display());
        println!("Data:   {}", report.paths.data.display());
        println!("Cache:  {}", report.paths.cache.display());
        println!("Runtime: {}", report.paths.runtime.display());
        println!("Stop background playback: yuin stop; close the service: yuin daemon stop");
    }
    Ok(())
}
