//! Running child processes with streaming output, timeouts and cancellation.
//!
//! Each child runs in its own process group. On timeout, cancellation or
//! exit the whole group is killed, so tools an agent started in the
//! background cannot outlive it.

use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdout, Command},
    sync::watch,
};

/// Bytes of stderr kept for error messages.
const STDERR_TAIL: usize = 4_000;

/// How long output is still read once the process has ended. A descendant
/// that left the process group (with `setsid`, for example) can keep the
/// pipes open for as long as it lives; the run must not wait for it.
const DRAIN: Duration = Duration::from_secs(2);

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
    /// Stop the process once it prints nothing on stdout for this long.
    pub idle: Option<Duration>,
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    /// Killed by a signal.
    Signal,
    TimedOut,
    /// Printed nothing for longer than `Spec::idle`.
    Stalled,
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

pub(crate) fn kill_group(child: &Child) {
    if let Some(pid) = child.id().and_then(|id| i32::try_from(id).ok()) {
        let group = nix::unistd::Pid::from_raw(pid);
        // The group may already be gone; there is nothing to do then.
        let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
    }
}

/// Start the process. A script that was just written can still be open for
/// writing in a child forked meanwhile by another thread (until that child
/// execs), and `exec` then fails with "Text file busy"; that passes, so it is
/// retried a few times.
async fn spawn(spec: &Spec) -> Result<Child> {
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
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 5 => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20 * attempt)).await;
            }
            other => return other.with_context(|| format!("cannot start {}", spec.program)),
        }
    }
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

/// Resolve once nothing was printed for `idle`; never without a limit.
/// `last` is when the latest line arrived, in milliseconds after `start`.
async fn silent(idle: Option<Duration>, start: Instant, last: &AtomicU64) {
    let Some(idle) = idle else {
        return std::future::pending().await;
    };
    loop {
        let seen = start + Duration::from_millis(last.load(Ordering::Relaxed));
        let quiet = seen.elapsed();
        if quiet >= idle {
            return;
        }
        tokio::time::sleep(idle - quiet).await;
    }
}

/// Run `spec`, passing every stdout line to `on_line` as it arrives.
///
/// The run ends when the process exits, not when its output closes: a
/// descendant outside the process group may hold the pipes for much longer.
pub async fn run(
    spec: &Spec,
    cancel: Option<Cancel>,
    on_line: &mut (dyn FnMut(&str) + Send),
) -> Result<Output> {
    let mut child = spawn(spec).await?;
    let (stdout, feeder, mut errors) =
        start_io(&mut child, spec.stdin.clone().unwrap_or_default())?;
    let ended = Box::pin(watch_child(&mut child, stdout, spec, cancel, on_line)).await;
    kill_group(&child);
    feeder.abort();
    if !exited(&ended) {
        // Reap the killed process so it does not linger as a zombie.
        let _ = tokio::time::timeout(DRAIN, child.wait()).await;
    }
    let stderr = match tokio::time::timeout(DRAIN, &mut errors).await {
        Ok(text) => text.unwrap_or_default(),
        Err(_) => {
            errors.abort();
            String::new()
        }
    };
    let stderr = tail(&stderr, STDERR_TAIL);
    ended.map(|exit| Output { exit, stderr })
}

/// The process ended by itself.
const fn exited(ended: &Result<Exit>) -> bool {
    matches!(ended, Ok(Exit::Code(_) | Exit::Signal))
}

/// Pass on stdout until the process exits, times out, falls silent or is
/// cancelled.
async fn watch_child(
    child: &mut Child,
    stdout: ChildStdout,
    spec: &Spec,
    cancel: Option<Cancel>,
    on_line: &mut (dyn FnMut(&str) + Send),
) -> Result<Exit> {
    let start = Instant::now();
    let last = AtomicU64::new(0);
    let mut seen = |line: &str| {
        let now = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        last.store(now, Ordering::Relaxed);
        on_line(line);
    };
    let mut pumping = Box::pin(pump(stdout, &mut seen));
    let mut deadline = Box::pin(tokio::time::sleep(spec.timeout));
    let mut closed = false;
    let ended = loop {
        tokio::select! {
            read = &mut pumping, if !closed => match read {
                Ok(()) => closed = true,
                Err(e) => break Err(e),
            },
            status = child.wait() => break Ok(status?.code().map_or(Exit::Signal, Exit::Code)),
            () = &mut deadline => break Ok(Exit::TimedOut),
            () = silent(spec.idle, start, &last) => break Ok(Exit::Stalled),
            () = cancelled(cancel.clone()) => break Ok(Exit::Cancelled),
        }
    };
    if exited(&ended) && !closed {
        // Read what is left; give up when a descendant keeps the pipe open.
        if let Ok(Err(e)) = tokio::time::timeout(DRAIN, &mut pumping).await {
            return Err(e);
        }
    }
    ended
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

    #[tokio::test]
    async fn ends_with_the_process_even_when_a_descendant_keeps_its_pipes() {
        let started = std::time::Instant::now();
        let mut lines = Vec::new();
        let out = run(&sh("setsid sleep 20 & echo hi", 20_000), None, &mut |l| {
            lines.push(l.to_owned());
        })
        .await
        .unwrap();
        assert!(out.success());
        assert_eq!(lines, ["hi"]);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn times_out_even_when_a_descendant_keeps_its_pipes() {
        let started = std::time::Instant::now();
        let out = run(&sh("setsid sleep 20 & sleep 30", 100), None, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(out.exit, Exit::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn stops_a_silent_process_but_not_a_chatty_one() {
        let quiet = Spec {
            idle: Some(Duration::from_millis(300)),
            ..sh("echo start; sleep 30", 20_000)
        };
        let started = std::time::Instant::now();
        let out = run(&quiet, None, &mut |_| {}).await.unwrap();
        assert_eq!(out.exit, Exit::Stalled);
        assert!(started.elapsed() < Duration::from_secs(10));
        let chatty = Spec {
            idle: Some(Duration::from_millis(300)),
            ..sh("for i in 1 2 3 4 5 6; do echo $i; sleep 0.1; done", 20_000)
        };
        assert!(run(&chatty, None, &mut |_| {}).await.unwrap().success());
    }

    #[test]
    fn tail_respects_char_boundaries() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("aé", 1), "");
        assert_eq!(tail("ab", 10), "ab");
    }
}
