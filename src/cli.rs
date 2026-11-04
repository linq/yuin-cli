use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "yuin", version, about = "Yuin · music in your terminal")]
#[command(
    after_help = "Project scaffold: catalog, music sources, playback, and downloads are planned.\nSee docs/TECHNICAL_DESIGN.zh-CN.md for the implementation plan."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Open the interactive terminal shell (navigation only in this scaffold)
    Tui,
    /// Report local paths and availability of the planned audio backend
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Show application paths without creating directories or modifying data
    Paths {
        #[arg(long)]
        json: bool,
    },
}
