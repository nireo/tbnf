mod app;
mod output;
mod preview;
mod terminal;
mod ui;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser};

use crate::{app::App, preview::Previewer, terminal::TerminalSession};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Show a file preview on the right (enabled by default)
    #[arg(short = 'p', long, action = ArgAction::SetTrue)]
    preview: bool,

    /// Hide the file preview
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "preview")]
    no_preview: bool,

    /// Directory to browse
    #[arg(value_name = "DIRECTORY")]
    directory: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let initial_dir = match args.directory {
        Some(path) if path.is_absolute() => path,
        Some(path) => std::env::current_dir()?.join(path),
        None => std::env::current_dir().context("could not determine current directory")?,
    };

    let mut app = App::new(initial_dir, !args.no_preview)?;
    let mut previewer = Previewer::new();
    let mut terminal = TerminalSession::new().context("could not initialize terminal")?;
    let exit = terminal.run(&mut app, &mut previewer);
    drop(terminal);

    if let Some(action) = exit? {
        println!("{}", output::format_action(&action));
    }
    Ok(())
}
