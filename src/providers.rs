use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn environment() -> HashMap<String, String> {
    let mut env: HashMap<_, _> = std::env::vars().collect();
    let mut paths: Vec<PathBuf> =
        std::env::split_paths(env.get("PATH").map(String::as_str).unwrap_or("")).collect();
    if let Some(home) = env.get("HOME") {
        for suffix in [".local/bin", ".npm-global/bin", ".bun/bin", ".cargo/bin"] {
            let p = Path::new(home).join(suffix);
            if p.is_dir() && !paths.contains(&p) {
                paths.insert(0, p);
            }
        }
    }
    if let Ok(path) = std::env::join_paths(paths) {
        env.insert("PATH".into(), path.to_string_lossy().into_owned());
    }
    env
}

pub fn executable(name: &str) -> bool {
    fn runnable(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            path.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
    if name.contains(std::path::MAIN_SEPARATOR) {
        return runnable(Path::new(name));
    }
    std::env::split_paths(environment().get("PATH").map(String::as_str).unwrap_or(""))
        .any(|p| runnable(&p.join(name)))
}

pub struct ProviderRunner<'a> {
    providers: &'a Value,
}
impl<'a> ProviderRunner<'a> {
    pub fn new(providers: &'a Value) -> Self {
        Self { providers }
    }
    pub fn available(&self, name: &str) -> (bool, String) {
        let cfg = &self.providers[name];
        if cfg["enabled"].as_bool() != Some(true) {
            return (false, "disabled in configuration".into());
        }
        if cfg["type"] == "ollama" {
            return (true, "endpoint configured; checked when called".into());
        }
        let Some(command) = cfg["command"][0].as_str() else {
            return (false, "command not configured".into());
        };
        let ok = executable(command);
        (
            ok,
            if ok {
                "found".into()
            } else {
                format!("executable not found: {command}")
            },
        )
    }
    pub fn run(
        &self,
        provider: &str,
        model: &str,
        prompt: &str,
        timeout: Duration,
        cwd: &Path,
        log: Option<&Path>,
    ) -> Result<String> {
        let cfg = &self.providers[provider];
        if cfg["enabled"].as_bool() != Some(true) {
            bail!("Provider '{provider}' unavailable or disabled");
        }
        if !cfg["models"]
            .as_array()
            .is_some_and(|xs| xs.iter().any(|v| v == model))
        {
            bail!("Model '{model}' not allowed for provider '{provider}'");
        }
        if cfg["type"] == "ollama" {
            let url = format!(
                "{}/api/generate",
                cfg["endpoint"]
                    .as_str()
                    .unwrap_or("http://localhost:11434")
                    .trim_end_matches('/')
            );
            let data: Value = reqwest::blocking::Client::builder()
                .timeout(timeout)
                .build()?
                .post(url)
                .json(&json!({"model":model,"prompt":prompt,"stream":false}))
                .send()?
                .error_for_status()?
                .json()?;
            let output = data["response"].as_str().unwrap_or("").trim().to_string();
            if output.is_empty() {
                bail!("Provider returned an empty response");
            }
            if let Some(path) = log {
                let _ = crate::joblog::append(path, &output);
            }
            return Ok(output);
        }
        if cfg["type"] != "cli" {
            bail!("Unsupported provider type: {}", cfg["type"]);
        }
        let args: Vec<String> = cfg["command"]
            .as_array()
            .context("Provider command must be an array")?
            .iter()
            .map(|s| {
                s.as_str()
                    .context("Command arguments must be strings")
                    .map(|s| s.replace("{model}", model))
            })
            .collect::<Result<_>>()?;
        if args.is_empty() {
            bail!("Provider command is empty");
        }
        let mut env = environment();
        if let Some(overrides) = cfg["env"].as_object() {
            for (k, v) in overrides {
                let value = v.as_str().context("Environment values must be strings")?;
                let value = if let Some(rest) = value.strip_prefix("~/") {
                    format!(
                        "{}/{rest}",
                        env.get("HOME").map(String::as_str).unwrap_or("")
                    )
                } else {
                    value.to_string()
                };
                env.insert(k.clone(), value);
            }
        }
        run_cli(&args, prompt, timeout, cwd, &env, log)
    }
}

fn pump(reader: impl std::io::Read, path: Option<PathBuf>) -> Vec<u8> {
    let mut result = Vec::new();
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Some(p) = &path {
                    let _ = crate::joblog::append(p, &String::from_utf8_lossy(&line));
                }
                result.extend(line);
            }
        }
    }
    result
}

pub fn run_cli(
    args: &[String],
    prompt: &str,
    timeout: Duration,
    cwd: &Path,
    env: &HashMap<String, String>,
    log: Option<&Path>,
) -> Result<String> {
    let mut command = Command::new(args.first().context("Empty command")?);
    command
        .args(&args[1..])
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Cannot launch {}", args[0]))?;
    let mut stdin = child.stdin.take().context("Missing agent stdin")?;
    let input = prompt.to_string();
    let feeder = thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let stdout = child.stdout.take().context("Missing stdout")?;
    let stderr = child.stderr.take().context("Missing stderr")?;
    let out_path = log.map(Path::to_path_buf);
    let err_path = out_path.clone();
    let out = thread::spawn(move || pump(stdout, out_path));
    let err = thread::spawn(move || pump(stderr, err_path));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = feeder.join();
            let _ = out.join();
            let _ = err.join();
            bail!("Timeout after {} seconds", timeout.as_secs_f64());
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Close inherited pipes from background descendants too: an agent may exit before its children.
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = feeder.join();
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))?;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))?;
    if !status.success() {
        let error = String::from_utf8_lossy(&stderr);
        let tail: String = error
            .chars()
            .rev()
            .take(3000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        bail!("CLI returned {}: {}", status, tail);
    }
    let output = String::from_utf8_lossy(&stdout).trim().to_string();
    if output.is_empty() {
        bail!("Provider returned an empty response");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(script: &str, prompt: &str, timeout: Duration) -> Result<String> {
        run_cli(
            &["sh".into(), "-c".into(), script.into()],
            prompt,
            timeout,
            Path::new("/tmp"),
            &environment(),
            None,
        )
    }
    #[test]
    fn stdin_and_stdout() {
        assert_eq!(
            run("cat", "hello", Duration::from_secs(2)).unwrap(),
            "hello"
        );
    }
    #[test]
    fn cwd() {
        assert!(
            run("pwd", "", Duration::from_secs(2))
                .unwrap()
                .ends_with("tmp")
        );
    }
    #[test]
    fn stderr_failure() {
        assert!(
            run("echo failure >&2; exit 3", "", Duration::from_secs(2))
                .unwrap_err()
                .to_string()
                .contains("failure")
        );
    }
    #[test]
    fn empty_response_fails() {
        assert!(run("true", "", Duration::from_secs(2)).is_err());
    }
    #[test]
    fn streams_stdout_and_stderr_to_log() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("agent.log");
        let output = run_cli(
            &[
                "sh".into(),
                "-c".into(),
                "echo answer; echo diagnostic >&2".into(),
            ],
            "",
            Duration::from_secs(2),
            dir.path(),
            &environment(),
            Some(&path),
        )?;
        assert_eq!(output, "answer");
        let log = std::fs::read_to_string(path)?;
        assert!(log.contains("answer"));
        assert!(log.contains("diagnostic"));
        Ok(())
    }
    #[test]
    fn timeout_kills_process_tree() {
        let now = Instant::now();
        assert!(
            run("sleep 20", "", Duration::from_millis(80))
                .unwrap_err()
                .to_string()
                .contains("Timeout")
        );
        assert!(now.elapsed() < Duration::from_secs(3));
    }
}
