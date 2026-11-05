use crate::{domain::Platform, services::ServiceCommand};
use clap::Subcommand;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum SourceCommand {
    Import { location: String },
    List,
    Use { id: Uuid },
    Update { id: Uuid },
    Remove { id: Uuid },
    Stop,
}
impl SourceCommand {
    pub fn service(self) -> ServiceCommand {
        match self {
            Self::Import { location } => ServiceCommand::SourceImport { location },
            Self::List => ServiceCommand::SourceList,
            Self::Use { id } => ServiceCommand::SourceUse { id },
            Self::Update { id } => ServiceCommand::SourceUpdate { id },
            Self::Remove { id } => ServiceCommand::SourceRemove { id },
            Self::Stop => ServiceCommand::SourceStop,
        }
    }
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    Get,
    Set { key: String, value: String },
}
impl ConfigCommand {
    pub fn service(self) -> ServiceCommand {
        match self {
            Self::Get => ServiceCommand::ConfigGet,
            Self::Set { key, value } => ServiceCommand::ConfigSet { key, value },
        }
    }
}

#[derive(Subcommand)]
pub enum CollectionCommand {
    /// Replace the queue with the entire collection and start playback
    Play { id: String },
    /// Save a complete collection once in My Library
    Favorite {
        id: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Search online playlists by name
    Search {
        query: String,
        #[arg(long)]
        platform: Option<Platform>,
        #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..=1000))]
        page: u32,
    },
    List {
        #[arg(long)]
        platform: Option<Platform>,
        #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..=1000))]
        page: u32,
    },
    Show {
        id: String,
        #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..=1000))]
        page: u32,
        #[arg(long)]
        all: bool,
    },
}
impl CollectionCommand {
    pub fn service(self, charts: bool) -> anyhow::Result<ServiceCommand> {
        Ok(match self {
            Self::Play { .. } => anyhow::bail!("Playback command must use the player dispatcher"),
            Self::Favorite { id, name } => ServiceCommand::LibraryFavorite {
                name: name.unwrap_or_else(|| id.clone()),
                collection: id,
                chart: charts,
            },
            Self::Search {
                query,
                platform,
                page,
            } => {
                anyhow::ensure!(!charts, "Search is available for playlists, not charts");
                ServiceCommand::SearchCollections {
                    query,
                    platform,
                    page,
                }
            }
            Self::List { platform, page } => ServiceCommand::Collections {
                platform,
                charts,
                page,
            },
            Self::Show { id, page, all } => ServiceCommand::CollectionTracks {
                id,
                chart: charts,
                page,
                all,
            },
        })
    }
}

#[derive(Subcommand)]
pub enum LibraryCommand {
    /// Replace the queue with this saved playlist and start playback
    Play {
        id: Uuid,
    },
    Favorite {
        track: String,
    },
    List,
    Create {
        name: String,
    },
    Show {
        id: Uuid,
        #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..=1000))]
        page: u32,
    },
    Add {
        id: Uuid,
        track: String,
    },
    Rename {
        id: Uuid,
        name: String,
    },
    Save {
        collection: String,
        name: String,
        #[arg(long)]
        chart: bool,
    },
    Remove {
        id: Uuid,
        index: usize,
    },
    Move {
        id: Uuid,
        from: usize,
        to: usize,
    },
    Delete {
        id: Uuid,
    },
}
impl LibraryCommand {
    pub fn service(self) -> anyhow::Result<ServiceCommand> {
        Ok(match self {
            Self::Play { .. } => anyhow::bail!("Playback command must use the player dispatcher"),
            Self::Favorite { track } => ServiceCommand::Favorite { track },
            Self::List => ServiceCommand::LibraryList,
            Self::Create { name } => ServiceCommand::LibraryCreate { name },
            Self::Show { id, page } => ServiceCommand::LibraryShow { id, page },
            Self::Add { id, track } => ServiceCommand::LibraryAdd { id, track },
            Self::Rename { id, name } => ServiceCommand::LibraryRename { id, name },
            Self::Save {
                collection,
                name,
                chart,
            } => ServiceCommand::LibrarySave {
                name,
                collection,
                chart,
            },
            Self::Remove { id, index } => ServiceCommand::LibraryRemove { id, index },
            Self::Move { id, from, to } => ServiceCommand::LibraryMove { id, from, to },
            Self::Delete { id } => ServiceCommand::LibraryDelete { id },
        })
    }
}

#[derive(Subcommand)]
pub enum DownloadCommand {
    List {
        #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..=1000))]
        page: u32,
    },
    Show {
        id: Uuid,
    },
    Cancel {
        id: Uuid,
    },
    Retry {
        id: Uuid,
    },
}
impl DownloadCommand {
    pub fn service(self) -> ServiceCommand {
        match self {
            Self::List { page } => ServiceCommand::Downloads { page },
            Self::Show { id } => ServiceCommand::DownloadShow { id },
            Self::Cancel { id } => ServiceCommand::DownloadCancel { id },
            Self::Retry { id } => ServiceCommand::DownloadRetry { id },
        }
    }
}
