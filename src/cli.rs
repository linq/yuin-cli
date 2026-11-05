use clap::{Parser, Subcommand};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "yuin", version, about = "Yuin · music in your terminal")]
#[command(
    after_help = "Install mpv for audio. Online tracks require an explicitly imported and enabled LX source.\nCLI and TUI share the same daemon, queue, library and download tasks."
)]
pub struct Cli {
    /// Emit a versioned JSON document (errors go to stderr)
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(hide = true)]
    SourceWorker { path: PathBuf },
    /// Search public track metadata
    Search {
        query: String,
        #[arg(long)]
        platform: Option<crate::domain::Platform>,
        #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..=1000))]
        page: u32,
    },
    /// Browse public playlists
    Playlists {
        #[command(subcommand)]
        command: crate::commands::CollectionCommand,
    },
    /// Browse platform charts
    Charts {
        #[command(subcommand)]
        command: crate::commands::CollectionCommand,
    },
    /// Manage trusted LX API 2.0 music sources
    Source {
        #[command(subcommand)]
        command: crate::commands::SourceCommand,
    },
    /// Inspect or atomically update configuration
    Config {
        #[command(subcommand)]
        command: crate::commands::ConfigCommand,
    },
    /// Manage personal playlists
    Library {
        #[command(subcommand)]
        command: crate::commands::LibraryCommand,
    },
    /// Enqueue a validated offline download
    Download {
        track: String,
        #[arg(long,value_parser=crate::config::QUALITIES)]
        quality: Option<String>,
        #[arg(long)]
        wait: bool,
    },
    /// Inspect, cancel or retry download tasks
    Downloads {
        #[command(subcommand)]
        command: crate::commands::DownloadCommand,
    },
    /// Fetch synchronized lyrics (cached for offline use)
    Lyrics {
        track: String,
        #[arg(long)]
        refresh: bool,
    },
    /// Preview a native or desktop JSON library export; --commit applies it with backup
    Import {
        path: PathBuf,
        #[arg(long)]
        commit: bool,
    },
    /// Export personal lists, history and offline index to a new private JSON file
    Export {
        path: PathBuf,
        #[arg(long)]
        include_sources: bool,
    },
    /// Generate shell completions without starting services
    Completions { shell: clap_complete::Shell },
    /// Render the yuin manual page to stdout
    Man,
    /// Show recent playback history
    History {
        #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..=1000))]
        page: u32,
    },
    /// Open the interactive terminal player
    Tui,
    /// Report local paths and availability of the audio backend (read-only)
    Doctor,
    /// Show application paths without creating directories or modifying data
    Paths,
    /// Play a local audio file or stable platform:id
    Play { path: PathBuf },
    /// Pause the shared player
    Pause,
    /// Resume the current entry, or start the queue
    Resume,
    /// Stop playback, keeping the queue
    Stop,
    /// Play the next queue entry
    Next,
    /// Play the previous queue entry
    Previous,
    /// Seek to an absolute position in seconds
    Seek {
        #[arg(value_parser = nonnegative)]
        seconds: f64,
    },
    /// Set volume from 0 through 100
    Volume {
        #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
        percent: u8,
    },
    /// Show a playback snapshot; restored audio stays paused
    Status,
    /// Choose sequential, shuffle or repeat-one playback
    Mode { mode: crate::domain::PlaybackMode },
    /// Manage the shared queue
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
    },
    /// Start or stop the per-user service
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Subcommand)]
pub enum QueueCommand {
    List,
    Add {
        path: PathBuf,
    },
    /// Play a specific queue entry UUID
    Play {
        id: Uuid,
    },
    Remove {
        id: Uuid,
    },
    Clear,
    Move {
        id: Uuid,
        to: usize,
    },
}

#[derive(Subcommand)]
pub enum DaemonCommand {
    Start,
    Stop,
    /// Run the service in the foreground (normally started automatically)
    #[command(hide = true)]
    Run,
}

fn nonnegative(text: &str) -> Result<f64, String> {
    let number = text
        .parse::<f64>()
        .map_err(|_| "Expected a number".to_owned())?;
    if !number.is_finite() || number < 0.0 {
        return Err("Expected finite, nonnegative seconds".into());
    }
    Ok(number)
}
