#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The `amaru` binary against a temporary copy of the example settings.

use std::{path::Path, process::Command};

fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let daemon = std::fs::read_to_string(examples.join("daemon.yaml")).unwrap();
    std::fs::write(
        dir.path().join("amaru.yaml"),
        daemon.replace("../data", "data"),
    )
    .unwrap();
    std::fs::copy(
        examples.join("project.yaml"),
        dir.path().join("project.yaml"),
    )
    .unwrap();
    dir
}

fn amaru(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_amaru"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn lists_commands() {
    let dir = setup();
    let (ok, text) = amaru(dir.path(), &["--help"]);
    assert!(ok);
    for command in [
        "serve", "claim", "drain", "status", "unblock", "guide", "cancel", "clean", "doctor",
    ] {
        assert!(text.contains(command), "{command}");
    }
}

#[test]
fn reports_status_and_rejects_bad_input() {
    let dir = setup();
    let (ok, text) = amaru(dir.path(), &["status"]);
    assert!(ok, "{text}");
    assert!(dir.path().join("data/amaru.db").exists());
    let (ok, text) = amaru(dir.path(), &["claim", "not-a-ref"]);
    assert!(!ok && text.contains("owner/name#number"), "{text}");
    let (ok, text) = amaru(dir.path(), &["claim", "other/repo#1"]);
    assert!(!ok && text.contains("not a configured project"), "{text}");
    let (ok, text) = amaru(dir.path(), &["cancel", "99"]);
    assert!(!ok && text.contains("no such ticket"), "{text}");
    let (ok, text) = amaru(dir.path(), &["guide", "99", "use", "ring"]);
    assert!(!ok && text.contains("no such ticket"), "{text}");
    let (ok, text) = amaru(dir.path(), &["guide", "99", "--file", "missing.md"]);
    assert!(!ok && text.contains("cannot read missing.md"), "{text}");
}

#[test]
fn rejects_invalid_settings() {
    let dir = setup();
    std::fs::write(
        dir.path().join("amaru.yaml"),
        "agents: {}\nprojects: []\nunknown: 1\n",
    )
    .unwrap();
    let (ok, text) = amaru(dir.path(), &["status"]);
    assert!(!ok && text.contains("unknown field"), "{text}");
}

#[test]
fn cleans_worktrees_no_ticket_uses() {
    let dir = setup();
    let old = dir.path().join("data/repos/me/app/worktrees/amaru-1-old");
    std::fs::create_dir_all(old.join("target")).unwrap();
    std::fs::write(old.join("target/big"), vec![0u8; 2048]).unwrap();
    let (ok, text) = amaru(dir.path(), &["clean", "--dry-run"]);
    assert!(
        ok && text.contains("would delete 1 worktree(s), 2.0 KB"),
        "{text}"
    );
    assert!(old.exists());
    let (ok, text) = amaru(dir.path(), &["clean"]);
    assert!(ok && text.contains("deleted 1 worktree(s)"), "{text}");
    assert!(!old.exists());
}
