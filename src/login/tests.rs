use super::*;
use crate::settings::DriverKind;

fn shell(login: &str, status: &str) -> AgentSettings {
    AgentSettings {
        driver: DriverKind::Codex,
        command: "sh".to_owned(),
        args: Vec::new(),
        model: None,
        env: std::iter::once(("GREETING".to_owned(), "hello".to_owned())).collect(),
        login: Some(vec!["-c".to_owned(), login.to_owned()]),
        login_with_key: None,
        login_status: Some(vec!["-c".to_owned(), status.to_owned()]),
        setup: Vec::new(),
        instructions: None,
    }
}

async fn ended(logins: &Logins, name: &str) -> LoginView {
    for _ in 0..200 {
        if logins.get(name).is_some_and(|v| !v.running) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let view = logins.get(name).unwrap();
    assert!(!view.running, "the sign-in did not end");
    view
}

#[tokio::test]
async fn passes_input_to_the_sign_in() {
    let logins = Logins::default();
    let agent = shell(
        "echo \"$GREETING, visit https://example.test\"; read code; echo \"got $code\"",
        "true",
    );
    let view = logins.start("codex", &agent, None).await.unwrap();
    assert!(view.running);
    logins.input("codex", "abc-123\n").await.unwrap();
    let view = ended(&logins, "codex").await;
    assert_eq!(view.exit.as_deref(), Some("finished"));
    assert!(
        view.output.contains("hello, visit https://example.test"),
        "{}",
        view.output
    );
    assert!(view.output.contains("got abc-123"), "{}", view.output);
    assert!(logins.input("codex", "late").await.is_err());
}

#[tokio::test]
async fn passes_the_key_and_closes_stdin() {
    let logins = Logins::default();
    let mut agent = shell("true", "true");
    // Like `codex login --with-api-key`: read to the end of input.
    agent.login_with_key = Some(vec![
        "-c".to_owned(),
        "key=$(cat); echo \"read ${#key} characters\"".to_owned(),
    ]);
    logins
        .start("codex", &agent, Some(" sk-secret-123 "))
        .await
        .unwrap();
    let view = ended(&logins, "codex").await;
    assert_eq!(view.exit.as_deref(), Some("finished"));
    assert!(
        view.output.contains("read 13 characters"),
        "{}",
        view.output
    );
    assert!(!view.output.contains("sk-secret"));
    agent.login_with_key = Some(Vec::new());
    assert!(logins.start("codex", &agent, Some("k")).await.is_err());
}

#[tokio::test]
async fn stops_a_sign_in() {
    let logins = Logins::default();
    logins
        .start("codex", &shell("sleep 30 & sleep 30", "true"), None)
        .await
        .unwrap();
    logins.cancel("codex").unwrap();
    assert_eq!(
        ended(&logins, "codex").await.exit.as_deref(),
        Some("stopped")
    );
    assert!(logins.cancel("claude").is_err());
}

#[tokio::test]
async fn reports_the_status() {
    let yes = status(&shell("true", "echo signed in as bot")).await;
    assert_eq!(yes.signed_in, Some(true));
    assert_eq!(yes.detail, "signed in as bot");
    let no = status(&shell("true", "echo not signed in >&2; exit 1")).await;
    assert_eq!(no.signed_in, Some(false));
    assert_eq!(no.detail, "not signed in");
    let mut missing = shell("true", "true");
    missing.command = "/nonexistent/agent".to_owned();
    assert_eq!(status(&missing).await.signed_in, None);
    assert!(Logins::default().start("x", &missing, None).await.is_err());
}

#[test]
fn defaults_follow_the_driver() {
    let mut agent = shell("", "");
    agent.login = None;
    agent.login_status = None;
    assert_eq!(agent.login_args(), ["login", "--device-auth"]);
    assert_eq!(agent.key_login_args(), ["login", "--with-api-key"]);
    assert_eq!(agent.status_args(), ["login", "status"]);
    agent.driver = DriverKind::Claude;
    assert_eq!(agent.login_args(), ["auth", "login"]);
    assert!(agent.key_login_args().is_empty());
    assert_eq!(agent.status_args(), ["auth", "status", "--text"]);
}

#[test]
fn cleans_terminal_output() {
    let raw = "\u{1b}[1;32mOpen\u{1b}[0m \u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{1b}\\ now\r\nspin\rdone";
    assert_eq!(clean(raw), "Open link now\nspin\ndone");
    assert_eq!(tail("héllo", 4), "llo");
}

#[tokio::test]
async fn an_agent_is_ready_only_when_signed_in() {
    let signed_in = readiness(&shell("true", "echo 'Logged in'")).await;
    assert!(signed_in.ready);
    assert_eq!(signed_in.detail, "Logged in");
    let signed_out = readiness(&shell("true", "echo 'Not logged in'; exit 1")).await;
    assert!(!signed_out.ready);
    assert_eq!(signed_out.detail, "Not logged in");
    let mut missing = shell("true", "true");
    missing.command = "/nonexistent/agent".to_owned();
    assert!(!readiness(&missing).await.ready);
    let mut unchecked = shell("true", "exit 1");
    unchecked.login_status = Some(Vec::new());
    assert!(readiness(&unchecked).await.ready);
}

#[tokio::test]
async fn sign_ins_report_only_changes() {
    let sign_ins = SignIns::default();
    let found = |ready| Readiness {
        ready,
        detail: String::new(),
        checked_at: String::new(),
    };
    assert!(sign_ins.ready("codex"));
    assert!(!sign_ins.record("codex", found(true)));
    assert!(sign_ins.record("codex", found(false)));
    assert!(!sign_ins.record("codex", found(false)));
    assert!(!sign_ins.ready("codex"));
    assert!(sign_ins.record("codex", found(true)));
    assert!(sign_ins.record("claude", found(false)));
    assert_eq!(sign_ins.all().len(), 2);
}
