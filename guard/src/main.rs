//! The `amaru-guard` command line.
//!
//! `amaru-guard check [-c FILE] TEXT|-` exits 0 when the text may reach the
//! agent, 1 when it must not, and 2 when it cannot decide.

use amaru_guard::{check, config::Config, download};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::{io::Read, path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(
    name = "amaru-guard",
    version,
    about = "Prompt injection guard for Amaru Light"
)]
struct Cli {
    /// The guard's settings file; the defaults without it.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download the model into the cache, unless it is there already.
    Download,
    /// Classify one text: exit 0 to pass it, 1 to reject it.
    Check {
        /// The text, or `-` to read it from stdin.
        text: String,
    },
}

/// The exit status when the guard cannot decide.
const ERROR: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("amaru-guard: {e:#}");
            ExitCode::from(ERROR)
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode> {
    let config = Config::load(cli.config.as_deref())?;
    match &cli.command {
        Command::Download => {
            let files = download::ensure(&config)?;
            eprintln!("amaru-guard: {} is ready", files.model.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Check { text } => {
            let text = if text == "-" { stdin()? } else { text.clone() };
            let decision = check(&config, &text)?;
            println!("{}", serde_json::to_string(&decision)?);
            Ok(if decision.rejected() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}

/// The text to check, when it is too long for an argument.
#[allow(clippy::disallowed_methods)] // A one-shot command, not the server.
fn stdin() -> Result<String> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("cannot read the text from stdin")?;
    Ok(text)
}
