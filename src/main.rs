mod cli;
mod doctor;
mod paths;
mod tui;

use clap::{CommandFactory, Parser};
use std::io::{self, IsTerminal};

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();
    match args.command {
        Some(cli::Command::Doctor { json }) => doctor::run(json),
        Some(cli::Command::Paths { json }) => {
            let paths = paths::AppPaths::discover()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&paths)?);
            } else {
                println!("Config: {}", paths.config.display());
                println!("Data:   {}", paths.data.display());
                println!("Cache:  {}", paths.cache.display());
            }
            Ok(())
        }
        Some(cli::Command::Tui) => tui::run(),
        None if io::stdin().is_terminal() && io::stdout().is_terminal() => tui::run(),
        None => {
            cli::Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}
