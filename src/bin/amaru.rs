#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! `amaru`: the v2 daemon and its command line.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use mini_amaru::{
    actions::{self, Claimed},
    app::services,
    forge::jira::{Curl, Jira},
    lifecycle::Services,
    process,
    scheduler::{Intake, Scheduler},
};
use std::{path::PathBuf, process::ExitCode, sync::Arc, time::Duration};
use tokio::sync::watch;

#[derive(Parser)]
#[command(
    name = "amaru",
    version,
    about = "Personal AI software delivery daemon"
)]
struct Cli {
    /// The daemon settings file.
    #[arg(short, long, global = true, default_value = "amaru.yaml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon and its web page: claim matching issues and keep every
    /// ticket moving.
    Serve,
    /// Queue one issue, `owner/name#42` or its URL; with --now, work on it
    /// right away.
    Claim {
        issue: String,
        #[arg(long)]
        now: bool,
    },
    /// Work through the open issues of a milestone, then stop.
    Drain {
        repo: String,
        #[arg(long)]
        milestone: String,
    },
    /// List recent tickets, or show one ticket's events.
    Status {
        #[arg(long)]
        ticket: Option<i64>,
    },
    /// Let a blocked ticket continue where it stopped.
    Unblock { ticket: i64 },
    /// Stop working on a ticket.
    Cancel { ticket: i64 },
    /// Check the tools and credentials the daemon needs.
    Doctor,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match Box::pin(dispatch(cli)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli) -> Result<()> {
    if matches!(cli.command, Command::Doctor) {
        return doctor(&cli.config).await;
    }
    let svc = services(&cli.config)?;
    match cli.command {
        Command::Serve => run(svc, Intake::Autonomous, true).await,
        Command::Claim { issue, now } => Box::pin(claim(svc, &issue, now)).await,
        Command::Drain { repo, milestone } => {
            run(svc, Intake::Milestone { repo, milestone }, false).await
        }
        Command::Status { ticket } => status(&svc, ticket),
        Command::Unblock { ticket } => resume(&svc, ticket).await,
        Command::Cancel { ticket } => cancel(&svc, ticket),
        Command::Doctor => Ok(()),
    }
}

/// Run the scheduler, and with `web` the web page, until the scheduler is
/// done or Ctrl-C is pressed.
async fn run(svc: Arc<Services>, intake: Intake, web: bool) -> Result<()> {
    let (stop, stop_rx) = watch::channel(false);
    let server = if web && svc.settings.daemon.web.enabled {
        Some(mini_amaru::web::start(Arc::clone(&svc), stop_rx.clone()).await?)
    } else {
        None
    };
    let ctrl_c = stop.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("stopping; unfinished tickets resume on the next start");
            let _ = ctrl_c.send(true);
        }
    });
    let result = Arc::new(Scheduler::new(svc, intake)).run(stop_rx).await;
    let _ = stop.send(true);
    if let Some(server) = server {
        // The server ends with the stop signal; a panic in it is not ours.
        let _ = server.await;
    }
    result
}

async fn claim(svc: Arc<Services>, reference: &str, now: bool) -> Result<()> {
    match actions::claim(&svc, reference).await? {
        Claimed::Queued(t) => println!("queued {} as ticket {}", t.reference(), t.id),
        Claimed::Exists(t) => println!(
            "{} already has an unfinished ticket: {} ({})",
            t.reference(),
            t.id,
            t.state
        ),
    }
    if now {
        return run(svc, Intake::Manual, false).await;
    }
    Ok(())
}

fn status(svc: &Services, ticket: Option<i64>) -> Result<()> {
    let Some(id) = ticket else {
        for t in svc.store.recent(30)? {
            let pr = t.pr_number.map(|n| format!(" PR #{n}")).unwrap_or_default();
            let reason = t
                .reason
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            println!("{:>4}  {:<18} {}{pr}{reason}", t.id, t.state, t.reference());
        }
        return Ok(());
    };
    let t = svc.store.get(id)?.context("no such ticket")?;
    println!("{} {}: {}\n", t.reference(), t.state, t.title);
    for event in svc.store.events(id, 0)? {
        println!("{}  {:<14} {}", event.at, event.kind, event.payload);
    }
    Ok(())
}

async fn resume(svc: &Services, id: i64) -> Result<()> {
    let ticket = actions::resume(svc, id).await?;
    println!("ticket {id} continues from {}", ticket.state);
    Ok(())
}

fn cancel(svc: &Services, id: i64) -> Result<()> {
    actions::cancel(svc, id)?;
    println!("ticket {id} cancelled; a running daemon stops it at once");
    Ok(())
}

/// Run a tool and report whether it works.
async fn probe(label: &str, program: &str, args: &[&str]) -> bool {
    let cwd = std::env::temp_dir();
    let result = process::capture(program, args, &cwd, Duration::from_secs(30)).await;
    match &result {
        Ok(out) => println!("ok    {label}: {}", out.lines().next().unwrap_or_default()),
        Err(e) => println!("FAIL  {label}: {e:#}"),
    }
    result.is_ok()
}

/// Reach every Jira server the projects use, with their tokens.
async fn check_jira(settings: &mini_amaru::settings::Settings) -> bool {
    let mut ok = true;
    let jira: Vec<_> = settings
        .projects
        .iter()
        .filter_map(|p| p.jira.clone())
        .collect();
    if !jira.is_empty() {
        ok &= probe("curl", "curl", &["--version"]).await;
    }
    let http = std::sync::Arc::new(Curl::default());
    for config in jira {
        let label = format!("jira {}", config.project);
        match Jira::new(config, http.clone()).check().await {
            Ok(text) => println!("ok    {label}: {text}"),
            Err(e) => {
                println!("FAIL  {label}: {e:#}");
                ok = false;
            }
        }
    }
    ok
}

async fn doctor(config: &std::path::Path) -> Result<()> {
    let settings = mini_amaru::settings::Settings::load(config)?;
    println!("ok    settings: {} project(s)", settings.projects.len());
    let mut ok = probe("git", "git", &["--version"]).await;
    ok &= probe("gh", &settings.daemon.gh, &["auth", "status"]).await;
    for (name, agent) in &settings.daemon.agents {
        ok &= probe(&format!("agent {name}"), &agent.command, &["--version"]).await;
    }
    ok &= check_jira(&settings).await;
    if !ok {
        bail!("some checks failed");
    }
    Ok(())
}
