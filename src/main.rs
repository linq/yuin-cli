#[cfg(unix)]
mod app;
#[cfg(unix)]
mod catalog;
mod cli;
#[cfg(unix)]
mod commands;
#[cfg(unix)]
mod config;
#[cfg(unix)]
mod daemon;
mod doctor;
mod domain;
#[cfg(unix)]
mod download;
#[cfg(unix)]
mod http;
#[cfg(unix)]
mod lyrics;
#[cfg(unix)]
mod media;
#[cfg(unix)]
mod media_controls;
#[cfg(unix)]
mod migration;
mod paths;
#[cfg(unix)]
mod playback;
mod protocol;
#[cfg(unix)]
mod secure;
#[cfg(unix)]
mod services;
#[cfg(unix)]
mod source;
#[cfg(unix)]
mod storage;
#[cfg(unix)]
mod tui;

use clap::{CommandFactory, Parser};
use std::io::{self, IsTerminal};

fn main() {
    let args = cli::Cli::parse();
    let json = args.json;
    let result = (|| -> anyhow::Result<()> {
        #[cfg(unix)]
        if let Some(cli::Command::SourceWorker { path }) = &args.command {
            return source::worker::run(path);
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        #[cfg(target_os = "macos")]
        if matches!(
            args.command,
            Some(cli::Command::Daemon {
                command: cli::DaemonCommand::Run
            })
        ) && config::Config::load(&paths::AppPaths::discover()?)?.media_keys
        {
            return media_controls::run_mac(&runtime, run(args));
        }
        runtime.block_on(run(args))
    })();
    if let Err(error) = result {
        let code = error
            .downcast_ref::<protocol::RpcError>()
            .map_or(1, |error| error.code);
        if json {
            eprintln!(
                "{}",
                serde_json::json!({ "schema_version": 1, "error": { "code": code, "message": error.to_string() } })
            );
        } else {
            eprintln!("Error: {error:#}");
        }
        std::process::exit(code);
    }
}

async fn run(args: cli::Cli) -> anyhow::Result<()> {
    let json = args.json;
    match args.command {
        #[cfg(unix)]
        Some(cli::Command::SourceWorker { path }) => {
            tokio::task::spawn_blocking(move || source::worker::run(&path)).await?
        }
        Some(cli::Command::Completions { shell }) => {
            clap_complete::generate(
                shell,
                &mut cli::Cli::command(),
                "yuin",
                &mut std::io::stdout(),
            );
            Ok(())
        }
        Some(cli::Command::Man) => {
            clap_mangen::Man::new(cli::Cli::command()).render(&mut std::io::stdout())?;
            Ok(())
        }
        Some(cli::Command::Doctor) => doctor::run(json),
        Some(cli::Command::Paths) => {
            let paths = paths::AppPaths::discover()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&paths)?);
            } else {
                println!("Config: {}", paths.config.display());
                println!("Data:   {}", paths.data.display());
                println!("Cache:  {}", paths.cache.display());
                println!("Runtime: {}", paths.runtime.display());
            }
            Ok(())
        }
        Some(cli::Command::Tui) => run_tui(json).await,
        None if io::stdin().is_terminal() && io::stdout().is_terminal() && !json => {
            run_tui(false).await
        }
        None => {
            cli::Cli::command().print_help()?;
            println!();
            Ok(())
        }
        #[cfg(unix)]
        Some(command) => run_business(command, json).await,
        #[cfg(not(unix))]
        _ => anyhow::bail!("Playback and daemon currently support macOS and Linux only"),
    }
}

async fn run_tui(json: bool) -> anyhow::Result<()> {
    anyhow::ensure!(!json, "The TUI does not support --json");
    #[cfg(unix)]
    {
        tui::run().await
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("The player currently supports macOS and Linux only")
    }
}

#[cfg(unix)]
async fn run_business(command: cli::Command, json: bool) -> anyhow::Result<()> {
    use cli::Command;
    use protocol::AppCommand;
    use services::ServiceCommand;
    let download_wait = matches!(&command, Command::Download { wait: true, .. });
    let service = match command {
        Command::Search {
            query,
            platform,
            page,
        } => Some(ServiceCommand::Search {
            query,
            platform,
            page,
        }),
        Command::Playlists { command } => Some(command.service(false)),
        Command::Charts { command } => Some(command.service(true)),
        Command::Source { command } => Some(command.service()),
        Command::Config { command } => Some(command.service()),
        Command::Library { command } => Some(command.service()),
        Command::Lyrics { track, refresh } => Some(ServiceCommand::Lyrics { track, refresh }),
        Command::Import { path, commit } => Some(ServiceCommand::Import {
            path: absolute(path)?,
            commit,
        }),
        Command::Export {
            path,
            include_sources,
        } => Some(ServiceCommand::Export {
            path: absolute(path)?,
            include_sources,
        }),
        Command::History { page } => Some(ServiceCommand::History { page }),
        Command::Download { track, quality, .. } => {
            Some(ServiceCommand::Download { track, quality })
        }
        Command::Downloads { command } => Some(command.service()),
        other => return run_playback(other, json).await,
    };
    let mut value = daemon::call_value(
        AppCommand::Service {
            command: service.expect("service command"),
        },
        true,
    )
    .await?;
    if download_wait {
        let id = value["download"]["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Download returned no task ID"))?
            .to_string();
        loop {
            let row = daemon::call_value(
                AppCommand::Service {
                    command: ServiceCommand::DownloadShow { id: id.parse()? },
                },
                false,
            )
            .await?;
            let record = &row["download"];
            match record["state"].as_str() {
                Some("completed") => {
                    value = serde_json::json!({"schema_version":1,"download":record});
                    break;
                }
                Some("failed" | "cancelled") => {
                    return Err(protocol::RpcError::new(
                        5,
                        record["error"]
                            .as_str()
                            .unwrap_or("Download failed or was cancelled"),
                    )
                    .into());
                }
                _ => {}
            }
            tokio::select! {
                _=tokio::time::sleep(std::time::Duration::from_millis(250))=>{},
                _=tokio::signal::ctrl_c()=>{let _=daemon::call_value(AppCommand::Service{command:ServiceCommand::DownloadCancel{id:id.parse()?}},false).await;return Err(protocol::RpcError::new(5,"Download cancelled").into());}
            }
        }
    }
    print_value(&value, json);
    Ok(())
}

#[cfg(unix)]
fn print_value(value: &serde_json::Value, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
        return;
    }
    if let Some(items) = value["items"].as_array() {
        for item in items {
            let id = item["id"].as_str().unwrap_or("—");
            let title = item["title"]
                .as_str()
                .or(item["name"].as_str())
                .or(item["track"]["title"].as_str())
                .unwrap_or("—");
            let artist = item["artist"]
                .as_str()
                .or(item["subtitle"].as_str())
                .or(item["track"]["artist"].as_str())
                .unwrap_or("");
            let state = item["state"]
                .as_str()
                .or(item["version"].as_str())
                .unwrap_or_default();
            let quality = item["quality"].as_str().unwrap_or_default();
            println!("{id}  {title}  {artist}  {state} {quality}");
        }
        if items.is_empty() {
            println!("No items.");
        }
        if value["has_more"] == true {
            eprintln!(
                "More results available; use --page {}",
                value["page"].as_u64().unwrap_or(1) + 1
            );
        }
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
    }
}

#[cfg(unix)]
async fn run_playback(command: cli::Command, json: bool) -> anyhow::Result<()> {
    use cli::{Command, DaemonCommand, QueueCommand};
    use protocol::AppCommand;
    let queue_list = matches!(
        command,
        Command::Queue {
            command: QueueCommand::List
        }
    );
    let (command, autostart) = match command {
        Command::Play { path } => (
            AppCommand::Play {
                path: absolute(path)?,
            },
            true,
        ),
        Command::Pause => (AppCommand::Pause, true),
        Command::Resume => (AppCommand::Resume, true),
        Command::Stop => (AppCommand::Stop, true),
        Command::Next => (AppCommand::Next, true),
        Command::Previous => (AppCommand::Previous, true),
        Command::Seek { seconds } => (AppCommand::Seek { seconds }, true),
        Command::Volume { percent } => (AppCommand::Volume { percent }, true),
        Command::Status => (AppCommand::Snapshot, true),
        Command::Mode { mode } => (AppCommand::Mode { mode }, true),
        Command::Queue { command } => (
            match command {
                QueueCommand::List => AppCommand::Snapshot,
                QueueCommand::Add { path } => AppCommand::QueueAdd {
                    path: absolute(path)?,
                },
                QueueCommand::Play { id } => AppCommand::PlayEntry { id },
                QueueCommand::Remove { id } => AppCommand::QueueRemove { id },
                QueueCommand::Clear => AppCommand::QueueClear,
                QueueCommand::Move { id, to } => AppCommand::QueueMove { id, to },
            },
            true,
        ),
        Command::Daemon {
            command: DaemonCommand::Run,
        } => return daemon::run().await,
        Command::Daemon {
            command: DaemonCommand::Start,
        } => (AppCommand::Snapshot, true),
        Command::Daemon {
            command: DaemonCommand::Stop,
        } => (AppCommand::Shutdown, false),
        _ => unreachable!(),
    };
    let snapshot = daemon::call(command, autostart).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else if queue_list {
        if snapshot.queue.is_empty() {
            println!("Queue is empty. Add local audio with `yuin queue add <path>`.");
        }
        for entry in &snapshot.queue {
            println!(
                "{} {}  {}",
                if snapshot.current == Some(entry.id) {
                    ">"
                } else {
                    " "
                },
                entry.id,
                entry.track.title
            );
        }
    } else {
        let title = snapshot
            .current_entry()
            .map_or("—", |entry| entry.track.title.as_str());
        println!(
            "{:?}  {}  {:.1}s  Volume {}%  Queue {}",
            snapshot.state,
            title,
            snapshot.position,
            snapshot.volume,
            snapshot.queue.len()
        );
        if let Some(error) = snapshot.error {
            eprintln!("Playback: {error}");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn absolute(path: std::path::PathBuf) -> anyhow::Result<String> {
    if path
        .to_str()
        .is_some_and(|s| crate::catalog::parse_id(s).is_ok())
    {
        return Ok(path.to_string_lossy().into_owned());
    }
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    path.into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("Audio path must be valid UTF-8"))
}
