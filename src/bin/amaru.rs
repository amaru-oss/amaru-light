#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! `amaru`: the v2 daemon and its command line.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use mini_amaru::{
    app::{parse_issue, services},
    lifecycle::{BLOCKED_LABEL, Services, unblock},
    process,
    scheduler::{Intake, Scheduler},
    ticket::{Origin, State},
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
    /// Run the daemon: claim matching issues and keep every ticket moving.
    Serve,
    /// Queue one issue, `owner/name#42`; with --now, work on it right away.
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
        Command::Claim { issue, now } => claim(svc, &issue, now).await,
        Command::Drain { repo, milestone } => {
            run(svc, Intake::Milestone { repo, milestone }, true).await
        }
        Command::Status { ticket } => status(&svc, ticket),
        Command::Unblock { ticket } => resume(&svc, ticket).await,
        Command::Cancel { ticket } => cancel(&svc, ticket),
        Command::Doctor => Ok(()),
    }
}

/// Run the scheduler until it is done or Ctrl-C is pressed.
async fn run(svc: Arc<Services>, intake: Intake, until_ctrl_c: bool) -> Result<()> {
    let (stop, stop_rx) = watch::channel(false);
    if until_ctrl_c {
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("stopping; unfinished tickets resume on the next start");
                let _ = stop.send(true);
            }
        });
    }
    Arc::new(Scheduler::new(svc, intake)).run(stop_rx).await
}

async fn claim(svc: Arc<Services>, reference: &str, now: bool) -> Result<()> {
    let (repo, number) = parse_issue(reference).context("expected owner/name#number")?;
    if svc.settings.project(&repo).is_none() {
        bail!("{repo} is not a configured project");
    }
    let issue = svc.source.issue(&repo, number).await?;
    match svc
        .store
        .claim(&repo, number, &issue.title, Origin::Manual)?
    {
        Some(ticket) => println!("queued {} as ticket {}", ticket.reference(), ticket.id),
        None => println!("{reference} already has an unfinished ticket"),
    }
    if now {
        return run(svc, Intake::Manual, true).await;
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
    let mut ticket = svc.store.get(id)?.context("no such ticket")?;
    if ticket.state != State::Blocked {
        bail!("ticket {id} is {}, not blocked", ticket.state);
    }
    svc.source
        .remove_label(&ticket.repo, ticket.number, BLOCKED_LABEL)
        .await?;
    unblock(&mut ticket);
    svc.store.save(&ticket)?;
    println!("ticket {id} continues from {}", ticket.state);
    Ok(())
}

fn cancel(svc: &Services, id: i64) -> Result<()> {
    let mut ticket = svc.store.get(id)?.context("no such ticket")?;
    ticket.state = State::Cancelled;
    svc.store.save(&ticket)?;
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

async fn doctor(config: &std::path::Path) -> Result<()> {
    let settings = mini_amaru::settings::Settings::load(config)?;
    println!("ok    settings: {} project(s)", settings.projects.len());
    let mut ok = probe("git", "git", &["--version"]).await;
    ok &= probe("gh", &settings.daemon.gh, &["auth", "status"]).await;
    for (name, agent) in &settings.daemon.agents {
        ok &= probe(&format!("agent {name}"), &agent.command, &["--version"]).await;
    }
    if !ok {
        bail!("some checks failed");
    }
    Ok(())
}
