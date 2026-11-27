use super::*;
use std::path::Path;

/// A daemon file whose agents are `sh`, with HOME and the config
/// directories inside `dir`.
fn settings(dir: &Path, agents: &str) -> Settings {
    std::fs::write(
        dir.join("project.yaml"),
        "repo: me/demo\nagent: one\nintake:\n  labels: [amaru]\n",
    )
    .unwrap();
    let daemon = format!("agents:\n{agents}projects: [project.yaml]\n");
    std::fs::write(dir.join("amaru.yaml"), daemon).unwrap();
    Settings::load(&dir.join("amaru.yaml")).unwrap()
}

fn agent(name: &str, driver: &str, home: &Path, extra: &str) -> String {
    format!(
        "  {name}:\n    driver: {driver}\n    command: sh\n    env:\n      HOME: {}\n{extra}",
        home.display()
    )
}

#[tokio::test]
async fn runs_the_steps_in_order_with_the_agent_env() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let steps = "    setup:\n      - [-c, 'echo first >> \"$HOME/log\"']\n      - [-c, 'echo \"$PWD\" >> \"$HOME/log\"']\n";
    let s = settings(dir.path(), &agent("one", "codex", &home, steps));
    assert_eq!(prepare(&s).await, 0);
    let log = std::fs::read_to_string(home.join("log")).unwrap();
    assert_eq!(log, format!("first\n{}\n", home.display()));
}

#[tokio::test]
async fn a_failed_step_is_counted_and_the_rest_still_run() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let steps = "    setup:\n      - [-c, 'exit 3']\n      - [-c, 'touch \"$HOME/after\"']\n";
    let s = settings(dir.path(), &agent("one", "claude", &home, steps));
    assert_eq!(prepare(&s).await, 1);
    assert!(home.join("after").exists());
}

#[tokio::test]
async fn installs_the_instructions_where_each_cli_reads_them() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let codex_home = dir.path().join("codex");
    std::fs::write(dir.path().join("claude.md"), "Be brief.\n").unwrap();
    std::fs::write(dir.path().join("codex.md"), "Run the checks.\n").unwrap();
    let claude = agent("one", "claude", &home, "    instructions: claude.md\n");
    let codex = format!(
        "{}      CODEX_HOME: {}\n    instructions: codex.md\n",
        agent("two", "codex", &home, ""),
        codex_home.display()
    );
    let s = settings(dir.path(), &format!("{claude}{codex}"));
    assert_eq!(prepare(&s).await, 0);
    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
    assert_eq!(read(home.join(".claude/CLAUDE.md")), "Be brief.\n");
    assert_eq!(read(codex_home.join("AGENTS.md")), "Run the checks.\n");

    // A changed file replaces the old copy at the next start.
    std::fs::write(dir.path().join("claude.md"), "Be thorough.\n").unwrap();
    assert_eq!(prepare(&s).await, 0);
    assert_eq!(read(home.join(".claude/CLAUDE.md")), "Be thorough.\n");
}

#[test]
fn a_missing_instructions_file_or_empty_step_fails_at_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("project.yaml"),
        "repo: me/demo\nagent: one\nintake:\n  labels: [amaru]\n",
    )
    .unwrap();
    for extra in ["    instructions: nowhere.md\n", "    setup:\n      - []\n"] {
        let daemon = format!(
            "agents:\n  one:\n    driver: codex\n    command: codex\n{extra}projects: [project.yaml]\n"
        );
        std::fs::write(dir.path().join("amaru.yaml"), daemon).unwrap();
        let err = Settings::load(&dir.path().join("amaru.yaml")).unwrap_err();
        assert!(format!("{err:#}").contains("agent 'one'"), "{err:#}");
    }
}
