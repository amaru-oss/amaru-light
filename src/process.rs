//! Running child processes with streaming output, timeouts and cancellation.
//!
//! Each child runs in its own process group. On timeout, cancellation or
//! exit the whole group is killed, so tools an agent started in the
//! background cannot outlive it.

use anyhow::{Context, Result};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdout, Command},
    sync::watch,
};

/// Bytes of stderr kept for error messages.
const STDERR_TAIL: usize = 4_000;

/// What to run.
#[derive(Clone, Debug, Default)]
pub struct Spec {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    /// Written to stdin, which is then closed.
    pub stdin: Option<String>,
    pub timeout: Duration,
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    /// Killed by a signal.
    Signal,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Output {
    pub exit: Exit,
    /// The last part of stderr.
    pub stderr: String,
}

impl Output {
    pub const fn success(&self) -> bool {
        matches!(self.exit, Exit::Code(0))
    }
}

/// A cancellation signal shared by everything working on one ticket.
pub type Cancel = watch::Receiver<bool>;

/// Keep the last `limit` bytes of `text`, on a character boundary.
pub fn tail(text: &str, limit: usize) -> String {
    let start = text.len().saturating_sub(limit);
    let start = (start..=text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    text[start..].to_owned()
}

fn kill_group(child: &Child) {
    if let Some(pid) = child.id().and_then(|id| i32::try_from(id).ok()) {
        let group = nix::unistd::Pid::from_raw(pid);
        // The group may already be gone; there is nothing to do then.
        let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
    }
}

fn spawn(spec: &Spec) -> Result<Child> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(&spec.env)
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    command
        .spawn()
        .with_context(|| format!("cannot start {}", spec.program))
}

async fn pump(stdout: ChildStdout, on_line: &mut (dyn FnMut(&str) + Send)) -> Result<()> {
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        on_line(&line);
    }
    Ok(())
}

async fn cancelled(cancel: Option<Cancel>) {
    let Some(mut cancel) = cancel else {
        return std::future::pending().await;
    };
    // A dropped sender means nobody can cancel any more.
    if cancel.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

type IoTasks = (
    ChildStdout,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<String>,
);

/// Start feeding stdin and collecting stderr in the background.
fn start_io(child: &mut Child, input: String) -> Result<IoTasks> {
    let mut stdin = child.stdin.take().context("missing stdin")?;
    let stdout = child.stdout.take().context("missing stdout")?;
    let mut stderr = child.stderr.take().context("missing stderr")?;
    let feeder = tokio::spawn(async move {
        // An agent may exit without reading all of its input.
        let _ = stdin.write_all(input.as_bytes()).await;
    });
    let errors = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        text
    });
    Ok((stdout, feeder, errors))
}

/// Run `spec`, passing every stdout line to `on_line` as it arrives.
pub async fn run(
    spec: &Spec,
    cancel: Option<Cancel>,
    on_line: &mut (dyn FnMut(&str) + Send),
) -> Result<Output> {
    let mut child = spawn(spec)?;
    let (stdout, feeder, errors) = start_io(&mut child, spec.stdin.clone().unwrap_or_default())?;
    let work = async {
        pump(stdout, on_line).await?;
        Ok::<_, anyhow::Error>(child.wait().await?)
    };
    let exit = tokio::select! {
        status = tokio::time::timeout(spec.timeout, work) => match status {
            Ok(status) => status?.code().map_or(Exit::Signal, Exit::Code),
            Err(_) => Exit::TimedOut,
        },
        () = cancelled(cancel) => Exit::Cancelled,
    };
    kill_group(&child);
    feeder.abort();
    let stderr = tail(&errors.await.unwrap_or_default(), STDERR_TAIL);
    Ok(Output { exit, stderr })
}

/// Run a shell command and return its exit and the tail of its combined output.
pub async fn shell(
    command: &str,
    cwd: PathBuf,
    timeout: Duration,
    cancel: Option<Cancel>,
) -> Result<(Exit, String)> {
    let spec = Spec {
        program: "sh".into(),
        args: vec!["-c".into(), format!("exec 2>&1; {command}")],
        cwd,
        timeout,
        ..Spec::default()
    };
    let mut text = String::new();
    let output = run(&spec, cancel, &mut |line| {
        text.push_str(line);
        text.push('\n');
    })
    .await?;
    Ok((output.exit, tail(&text, STDERR_TAIL)))
}

/// Run a program to completion and return its stdout; fail on a non-zero exit.
pub async fn capture(
    program: &str,
    args: &[&str],
    cwd: &std::path::Path,
    timeout: Duration,
) -> Result<String> {
    let spec = Spec {
        program: program.into(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        cwd: cwd.to_path_buf(),
        timeout,
        ..Spec::default()
    };
    let mut text = String::new();
    let output = run(&spec, None, &mut |line| {
        text.push_str(line);
        text.push('\n');
    })
    .await?;
    if !output.success() {
        anyhow::bail!(
            "{program} {} failed ({:?}): {}",
            args.join(" "),
            output.exit,
            output.stderr.trim()
        );
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, timeout_ms: u64) -> Spec {
        Spec {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            stdin: Some("hello".into()),
            timeout: Duration::from_millis(timeout_ms),
            ..Spec::default()
        }
    }

    #[tokio::test]
    async fn streams_lines_and_feeds_stdin() {
        let mut lines = Vec::new();
        let out = run(
            &sh("cat; echo; echo two; echo oops >&2", 5_000),
            None,
            &mut |l| {
                lines.push(l.to_owned());
            },
        )
        .await
        .unwrap();
        assert!(out.success());
        assert_eq!(lines, ["hello", "two"]);
        assert_eq!(out.stderr.trim(), "oops");
    }

    #[tokio::test]
    async fn reports_exit_codes() {
        let out = run(&sh("exit 3", 5_000), None, &mut |_| {}).await.unwrap();
        assert_eq!(out.exit, Exit::Code(3));
    }

    #[tokio::test]
    async fn times_out_and_kills_the_group() {
        let started = std::time::Instant::now();
        let out = run(&sh("sleep 30 & sleep 30", 100), None, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(out.exit, Exit::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn stops_when_cancelled() {
        let (tx, rx) = watch::channel(false);
        let task =
            tokio::spawn(async move { run(&sh("sleep 30", 30_000), Some(rx), &mut |_| {}).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(true).unwrap();
        assert_eq!(task.await.unwrap().unwrap().exit, Exit::Cancelled);
    }

    #[tokio::test]
    async fn shell_merges_output() {
        let (exit, text) = shell(
            "echo out; echo err >&2; exit 1",
            std::env::temp_dir(),
            Duration::from_secs(5),
            None,
        )
        .await
        .unwrap();
        assert_eq!(exit, Exit::Code(1));
        assert_eq!(text, "out\nerr\n");
    }

    #[test]
    fn tail_respects_char_boundaries() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("aé", 1), "");
        assert_eq!(tail("ab", 10), "ab");
    }
}
