#![allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use mini_amaru::{
    config::Settings, delivery, engine::resolve_model, providers::ProviderRunner, runtime, server,
    storage::Store,
};
use std::{
    path::PathBuf,
    process::{Command, ExitCode},
};

#[derive(Parser)]
#[command(
    name = "mini-amaru",
    version,
    about = "Local engineering agent runtime"
)]
struct Cli {
    /// Project YAML configuration.
    #[arg(short, long, global = true, default_value = "projects/amaru.yaml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Plan and execute one engineering request.
    Run {
        #[arg(short, long)]
        prompt: String,
    },
    /// Start a new job continuing an earlier request.
    Resume {
        #[arg(long)]
        job: i64,
    },
    /// Check configured paths, tools and providers.
    Doctor,
    /// Run the persisted delivery loop.
    Loop {
        #[arg(short, long)]
        prompt: Option<String>,
        #[arg(long)]
        milestone: Option<String>,
        #[arg(long)]
        resume: Option<i64>,
        #[arg(long)]
        dry_run: bool,
    },
    Status {
        #[arg(long)]
        job: i64,
    },
    Jobs,
    /// Serve the JSON API used by the React dashboard.
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8765)]
        port: u16,
        #[arg(long = "cors-origin")]
        cors_origins: Vec<String>,
        /// Directory containing the built frontend (index.html and assets).
        #[arg(long)]
        web_dir: Option<PathBuf>,
    },
}

fn print_job(store: &Store, id: i64) -> Result<()> {
    let job = store.job(id)?.context("job not found")?;
    println!(
        "Job #{id} — {}\n{}\n{}",
        job["status"].as_str().unwrap_or(""),
        job["prompt"].as_str().unwrap_or(""),
        job["report"].as_str().unwrap_or("")
    );
    for task in store.tasks(id)? {
        println!(
            "  [{}] {}",
            task["status"].as_str().unwrap_or(""),
            task["title"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

fn job_exit(settings: &Settings, id: i64) -> Result<u8> {
    let store = Store::open(settings.db_path())?;
    let job = store.job(id)?.context("job not found")?;
    let status = job["status"].as_str().unwrap_or("FAILED");
    println!("Job {id}: {status}");
    Ok(
        if matches!(
            status,
            "COMPLETED" | "DONE" | "WAITING" | "WAITING_MERGE" | "WAITING_PR"
        ) {
            0
        } else {
            2
        },
    )
}

fn git_output(settings: &Settings, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(settings.project_root())
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}
fn doctor(settings: &Settings) {
    println!(
        "Project: {}\nDirectory: {}",
        settings.project_name(),
        settings.project_root().display()
    );
    let runner = ProviderRunner::new(&settings.providers);
    if let Some(providers) = settings.providers.as_object() {
        for name in providers.keys() {
            let (ok, why) = runner.available(name);
            println!("[{}] {name}: {why}", if ok { "OK" } else { "--" });
        }
    }
    match resolve_model(settings, "planning") {
        Ok((key, spec)) => println!(
            "[OK] planning: {key} ({})",
            spec["provider"].as_str().unwrap_or_default()
        ),
        Err(error) => println!("[--] {error}"),
    }
    println!(
        "[{}] project directory",
        if settings.project_root().is_dir() {
            "OK"
        } else {
            "--"
        }
    );
    println!(
        "[{}] Git branch: {}",
        if git_output(settings, &["rev-parse", "--git-dir"]).is_some() {
            "OK"
        } else {
            "--"
        },
        git_output(settings, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default()
    );
    for name in ["AGENTS.md", "CLAUDE.md"] {
        println!(
            "[{}] {name}",
            if settings.project_root().join(name).is_file() {
                "OK"
            } else {
                "--"
            }
        );
    }
    println!(
        "[{}] remote origin",
        if git_output(settings, &["remote", "get-url", "origin"]).is_some() {
            "OK"
        } else {
            "--"
        }
    );
    let auth = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .is_ok_and(|o| o.status.success());
    println!("[{}] gh authenticated", if auth { "OK" } else { "--" });
    println!(
        "Check command: {}",
        settings.project["execution"]["check_command"]
            .as_str()
            .unwrap_or("not configured")
    );
    println!(
        "Policy: merge={} publish={}",
        settings.project["merge"]["mode"]
            .as_str()
            .unwrap_or("integration_only"),
        settings.project["publish"]["mode"]
            .as_str()
            .unwrap_or("auto")
    );
}

fn run() -> Result<u8> {
    let cli = Cli::parse();
    let settings = Settings::load(&cli.config)?;
    match cli.command {
        Commands::Run { prompt } => {
            let id = runtime::run_job(&settings, &prompt, None)?;
            job_exit(&settings, id)
        }
        Commands::Resume { job } => {
            let store = Store::open(settings.db_path())?;
            let old = store.job(job)?.context("job not found")?;
            let prompt = runtime::resume_prompt(old["prompt"].as_str().unwrap_or(""), job);
            let id = runtime::run_job(&settings, &prompt, None)?;
            job_exit(&settings, id)
        }
        Commands::Doctor => {
            doctor(&settings);
            Ok(0)
        }
        Commands::Loop {
            prompt,
            milestone,
            resume,
            dry_run,
        } => {
            if let Some(id) = resume {
                if dry_run {
                    bail!("--resume and --dry-run cannot be combined");
                }
                let store = Store::open(settings.db_path())?;
                let job = store.job(id)?.context("loop job not found")?;
                if job["kind"] != "loop" {
                    bail!("job {id} is not a loop");
                }
                delivery::run_loop(
                    &settings,
                    job["prompt"].as_str().unwrap_or(""),
                    Some(id),
                    None,
                )?;
                return job_exit(&settings, id);
            }
            let goal = prompt
                .or_else(|| {
                    milestone.as_ref().map(|m| {
                        format!("Continue the work of Milestone {m} until it is completed.")
                    })
                })
                .context("provide --prompt or --milestone")?;
            if dry_run {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&delivery::dry_run(
                        &settings,
                        &goal,
                        milestone.as_deref()
                    )?)?
                );
                Ok(0)
            } else {
                let id = delivery::run_loop(&settings, &goal, None, milestone.as_deref())?;
                job_exit(&settings, id)
            }
        }
        Commands::Status { job } => {
            print_job(&Store::open(settings.db_path())?, job)?;
            Ok(0)
        }
        Commands::Jobs => {
            for job in Store::open(settings.db_path())?.jobs()? {
                println!(
                    "#{} [{}] {} — {}",
                    job["id"],
                    job["status"].as_str().unwrap_or(""),
                    job["project"].as_str().unwrap_or(""),
                    job["prompt"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        Commands::Serve {
            host,
            port,
            cors_origins,
            web_dir,
        } => {
            let origins = if cors_origins.is_empty() {
                vec![
                    "http://localhost:5173".into(),
                    "http://127.0.0.1:5173".into(),
                ]
            } else {
                cors_origins
            };
            server::serve(settings, &host, port, origins, web_dir)?;
            Ok(0)
        }
    }
}
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
