//! Signing agent CLIs in from the web page.
//!
//! Amaru does not ship the agent CLIs, nor their accounts. It runs the
//! CLI's own sign-in command, shows its output (the link and code to open
//! on another device) and passes a line of input back when the CLI asks
//! for one. The CLI stores its credentials where it always does.

use crate::settings::AgentSettings;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::Notify,
};

/// Output kept for each sign-in, from the end.
const MAX_OUTPUT: usize = 32 * 1024;
/// A sign-in still running after this long is stopped.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How long the status command may take.
const STATUS_TIMEOUT: Duration = Duration::from_secs(20);

/// What the page shows of a sign-in.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LoginView {
    pub running: bool,
    pub output: String,
    /// How the command ended, once it has.
    pub exit: Option<String>,
    pub started_at: String,
}

/// Whether an agent CLI is signed in.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LoginStatus {
    /// `None` when the status command could not run.
    pub signed_in: Option<bool>,
    pub detail: String,
}

#[derive(Debug)]
struct Progress {
    output: Vec<u8>,
    exit: Option<String>,
    started_at: String,
}

#[derive(Debug)]
pub(crate) struct Login {
    progress: Mutex<Progress>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    stop: Notify,
}

impl Login {
    fn progress(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn append(&self, bytes: &[u8]) {
        let mut p = self.progress();
        p.output.extend_from_slice(bytes);
        if p.output.len() > MAX_OUTPUT {
            let cut = p.output.len() - MAX_OUTPUT;
            p.output.drain(..cut);
        }
    }

    pub(crate) fn view(&self) -> LoginView {
        let p = self.progress();
        LoginView {
            running: p.exit.is_none(),
            output: clean(&String::from_utf8_lossy(&p.output)),
            exit: p.exit.clone(),
            started_at: p.started_at.clone(),
        }
    }
}

/// The sign-ins started from the page, one at most per agent.
#[derive(Debug, Default)]
pub struct Logins {
    by_agent: Mutex<HashMap<String, Arc<Login>>>,
}

impl Logins {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Login>>> {
        self.by_agent.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn find(&self, name: &str) -> Option<Arc<Login>> {
        self.map().get(name).cloned()
    }

    /// Start signing `name` in, stopping a sign-in already running for it.
    /// With `key`, run the API-key sign-in instead: the key goes to stdin,
    /// which is then closed. The key is never kept or shown.
    pub async fn start(
        &self,
        name: &str,
        agent: &AgentSettings,
        key: Option<&str>,
    ) -> Result<LoginView> {
        let args = match key {
            Some(_) => agent.key_login_args(),
            None => agent.login_args(),
        };
        if args.is_empty() {
            let how = if key.is_some() {
                "with an API key "
            } else {
                ""
            };
            bail!("signing in {how}from the page is turned off for agent {name}");
        }
        let login = self.run(name, command(agent, &args).stdin(Stdio::piped()))?;
        if let Some(key) = key {
            send_key(&login, key).await?;
        }
        Ok(login.view())
    }

    /// Run `cmd` for `name`, stopping the command already running for it, and
    /// keep its output for the page.
    pub(crate) fn run(&self, name: &str, cmd: &mut Command) -> Result<Arc<Login>> {
        self.run_then(name, cmd, None)
    }

    /// Like `run`, and once the command has finished well, run `then`
    /// before the page sees it end. `then` may add to the output; what it
    /// returns, when not empty, is added to how the command ended.
    pub(crate) fn run_then(
        &self,
        name: &str,
        cmd: &mut Command,
        then: Option<Then>,
    ) -> Result<Arc<Login>> {
        let line = format!("{cmd:?}");
        let child = cmd.spawn().with_context(|| format!("cannot run {line}"))?;
        let login = watch(child, then);
        if let Some(old) = self.map().insert(name.to_owned(), Arc::clone(&login)) {
            old.stop.notify_one();
        }
        Ok(login)
    }

    /// The latest sign-in for `name`, if any.
    pub fn get(&self, name: &str) -> Option<LoginView> {
        self.find(name).map(|l| l.view())
    }

    /// Send one line to the running sign-in, such as a pasted code.
    pub async fn input(&self, name: &str, text: &str) -> Result<()> {
        let login = self.find(name).context("no sign-in is running")?;
        let mut stdin = login.stdin.lock().await;
        let pipe = stdin.as_mut().context("the sign-in has ended")?;
        let line = format!("{}\n", text.trim_end_matches(['\r', '\n']));
        pipe.write_all(line.as_bytes()).await?;
        pipe.flush().await?;
        Ok(())
    }

    /// Stop the running sign-in for `name`.
    pub fn cancel(&self, name: &str) -> Result<()> {
        let login = self.find(name).context("no sign-in is running")?;
        login.stop.notify_one();
        Ok(())
    }
}

/// Write the key and close stdin, so that the CLI sees the end of input.
async fn send_key(login: &Login, key: &str) -> Result<()> {
    let pipe = login.stdin.lock().await.take();
    let mut pipe = pipe.context("the sign-in has no input")?;
    let line = format!("{}\n", key.trim());
    pipe.write_all(line.as_bytes())
        .await
        .context("cannot pass the key to the sign-in")?;
    pipe.shutdown().await.ok();
    Ok(())
}

/// Run the agent's status command.
pub async fn status(agent: &AgentSettings) -> LoginStatus {
    let args = agent.status_args();
    if args.is_empty() {
        return LoginStatus {
            signed_in: None,
            detail: "no status command is set".to_owned(),
        };
    }
    let run = command(agent, &args).stdin(Stdio::null()).output();
    let (signed_in, detail) = match tokio::time::timeout(STATUS_TIMEOUT, run).await {
        Ok(Ok(out)) => {
            let text = [out.stdout, out.stderr].concat();
            (
                Some(out.status.success()),
                clean(&String::from_utf8_lossy(&text)),
            )
        }
        Ok(Err(e)) => (None, format!("cannot run {}: {e}", agent.command)),
        Err(_) => (None, format!("{} did not answer in time", agent.command)),
    };
    LoginStatus {
        signed_in,
        detail: tail(detail.trim(), 2_000),
    }
}

/// Whether an agent can take work, as the scheduler last found it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Readiness {
    /// Signed in, or no status command is set to tell.
    pub ready: bool,
    pub detail: String,
    pub checked_at: String,
}

/// Ask the agent CLI whether it is signed in. A CLI that is not installed is
/// not ready; an installed one whose status command is turned off is; one whose status command fails,
/// cannot run or does not answer is not, because its turns would fail too.
pub async fn readiness(agent: &AgentSettings) -> Readiness {
    let (ready, detail) = if !crate::install::installed(agent) {
        let detail = format!(
            "{} is not installed; install it on the Agents page",
            agent.command
        );
        (false, detail)
    } else if agent.status_args().is_empty() {
        (true, "no status command is set".to_owned())
    } else {
        let found = status(agent).await;
        let detail = if found.detail.is_empty() {
            match found.signed_in {
                Some(true) => "signed in".to_owned(),
                _ => "not signed in".to_owned(),
            }
        } else {
            found.detail
        };
        (found.signed_in == Some(true), detail)
    };
    Readiness {
        ready,
        detail,
        checked_at: crate::db::now(),
    }
}

/// The latest readiness of each agent, shared by the scheduler, which
/// checks it, and the web page, which shows it.
#[derive(Debug, Default)]
pub struct SignIns {
    by_agent: Mutex<BTreeMap<String, Readiness>>,
}

impl SignIns {
    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Readiness>> {
        self.by_agent.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keep the latest check of `name`. True when it changes whether the
    /// agent can take work, or is the first check and finds it cannot.
    pub fn record(&self, name: &str, found: Readiness) -> bool {
        let ready = found.ready;
        let before = self.map().insert(name.to_owned(), found);
        before.map_or(!ready, |b| b.ready != ready)
    }

    /// Whether `name` can take work. An agent not checked yet can.
    pub fn ready(&self, name: &str) -> bool {
        self.map().get(name).is_none_or(|r| r.ready)
    }

    /// Every agent checked so far.
    pub fn all(&self) -> BTreeMap<String, Readiness> {
        self.map().clone()
    }
}

fn command(agent: &AgentSettings, args: &[String]) -> Command {
    program(&agent.command, args, &agent.env)
}

/// A command for the page to run: its output piped, in its own process group.
pub(crate) fn program(program: &str, args: &[String], env: &BTreeMap<String, String>) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .envs(env)
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    cmd
}

/// Collect the child's output and wait for it to end, be stopped or time out.
fn watch(mut child: Child, then: Option<Then>) -> Arc<Login> {
    let login = Arc::new(Login {
        progress: Mutex::new(Progress {
            output: Vec::new(),
            exit: None,
            started_at: crate::db::now(),
        }),
        stdin: tokio::sync::Mutex::new(child.stdin.take()),
        stop: Notify::new(),
    });
    let readers = [
        child
            .stdout
            .take()
            .map(|o| tokio::spawn(collect(o, Arc::clone(&login)))),
        child
            .stderr
            .take()
            .map(|e| tokio::spawn(collect(e, Arc::clone(&login)))),
    ];
    tokio::spawn(finish(child, Arc::clone(&login), readers, then));
    login
}

/// Work to do after a command has finished well, given its output to add to.
pub(crate) type Then = Box<
    dyn FnOnce(Arc<Login>) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>
        + Send,
>;

type Reader = Option<tokio::task::JoinHandle<()>>;

/// Record how the sign-in ended once its output is read.
async fn finish(mut child: Child, login: Arc<Login>, readers: [Reader; 2], then: Option<Then>) {
    let mut exit = wait(&mut child, &login).await;
    for reader in readers.into_iter().flatten() {
        // A process the CLI left behind may hold the pipes open.
        let abort = reader.abort_handle();
        if tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .is_err()
        {
            abort.abort();
        }
    }
    *login.stdin.lock().await = None;
    if let Some(then) = then.filter(|_| exit == "finished") {
        let note = then(Arc::clone(&login)).await;
        if !note.is_empty() {
            exit = format!("{exit}, {note}");
        }
    }
    login.progress().exit = Some(exit);
}

/// Wait for the child to end, be stopped or time out, and say which.
async fn wait(child: &mut Child, login: &Login) -> String {
    let stopped = tokio::select! {
        status = child.wait() => return status.map_or_else(|e| format!("failed: {e}"), describe),
        () = login.stop.notified() => "stopped",
        () = tokio::time::sleep(LOGIN_TIMEOUT) => "timed out",
    };
    crate::process::kill_group(child);
    let _ = child.kill().await;
    stopped.to_owned()
}

async fn collect(mut from: impl AsyncRead + Unpin, login: Arc<Login>) {
    let mut buf = [0u8; 4096];
    while let Ok(n) = from.read(&mut buf).await {
        if n == 0 {
            break;
        }
        login.append(&buf[..n]);
    }
}

fn describe(status: ExitStatus) -> String {
    match status.code() {
        Some(0) => "finished".to_owned(),
        Some(code) => format!("failed with exit code {code}"),
        None => "ended by a signal".to_owned(),
    }
}

/// Drop terminal escape sequences and carriage returns.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => skip_escape(&mut chars),
            '\r' if chars.peek() != Some(&'\n') => out.push('\n'),
            '\r' => {}
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        // CSI: parameters, then one final byte from @ to ~.
        Some('[') => {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        // OSC, such as a hyperlink: up to BEL or ESC \.
        Some(']') => {
            while let Some(c) = chars.next() {
                if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                    break;
                }
            }
        }
        _ => {}
    }
}

/// The last `limit` bytes of `text`, cut at a character boundary.
fn tail(text: &str, limit: usize) -> String {
    let start = (text.len().saturating_sub(limit)..=text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    text[start..].to_owned()
}

#[cfg(test)]
mod tests;
