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
        "serve", "claim", "drain", "status", "unblock", "cancel", "doctor",
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
