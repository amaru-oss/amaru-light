use super::*;
use std::sync::Mutex;

/// Answers GET requests from a list of (URL part, JSON) and records POSTs.
#[derive(Debug, Default)]
struct FakeHttp {
    answers: Vec<(&'static str, Value)>,
    gets: Mutex<Vec<(String, Auth)>>,
    posts: Mutex<Vec<(String, Value)>>,
    refuse_posts: bool,
}

#[async_trait]
impl Http for FakeHttp {
    async fn get(&self, url: &str, auth: &Auth) -> Result<Value> {
        self.gets
            .lock()
            .unwrap()
            .push((url.to_owned(), auth.clone()));
        self.answers
            .iter()
            .find(|(part, _)| url.contains(part))
            .map(|(_, v)| v.clone())
            .with_context(|| format!("no answer for {url}"))
    }
    async fn post(&self, url: &str, _: &Auth, body: &Value) -> Result<Value> {
        anyhow::ensure!(!self.refuse_posts, "403 Forbidden");
        self.posts
            .lock()
            .unwrap()
            .push((url.to_owned(), body.clone()));
        Ok(json!({}))
    }
}

fn settings() -> JiraSettings {
    serde_yaml::from_str("url: https://issues.example.org/jira/\nproject: CAMEL\n").unwrap()
}

fn status(category: &str) -> Value {
    json!({ "statusCategory": { "key": category } })
}

fn raw_issue() -> Value {
    json!({
        "key": "CAMEL-42",
        "fields": {
            "summary": "camel-kafka: support headers",
            "description": "Please add it.",
            "labels": ["kafka"],
            "assignee": { "name": "orpiske", "displayName": "Otavio" },
            "fixVersions": [{ "name": "4.10.0", "releaseDate": "2026-12-01" }],
            "created": "2026-11-01T10:00:00.000+0000",
            "status": status("indeterminate"),
            "issuelinks": [
                { "type": { "inward": "is blocked by", "outward": "blocks" },
                  "inwardIssue": { "key": "CAMEL-40", "fields": { "status": status("new") } } },
                { "type": { "inward": "is blocked by", "outward": "blocks" },
                  "inwardIssue": { "key": "CAMEL-39", "fields": { "status": status("done") } } },
                { "type": { "inward": "is depended upon by", "outward": "depends upon" },
                  "outwardIssue": { "key": "CAMEL-41", "fields": { "status": status("new") } } },
                { "type": { "inward": "is blocked by", "outward": "blocks" },
                  "outwardIssue": { "key": "CAMEL-50", "fields": { "status": status("new") } } },
                { "type": { "inward": "is blocked by", "outward": "blocks" },
                  "inwardIssue": { "key": "KAFKA-1", "fields": { "status": status("new") } } }
            ],
            "comment": { "comments": [
                { "author": { "name": "davsclaus", "displayName": "Claus" },
                  "created": "2026-11-02T10:00:00.000+0000", "body": "Use the existing mapper." }
            ] }
        }
    })
}

fn jira(http: &Arc<FakeHttp>, auth: Auth) -> Jira {
    Jira::with_auth(settings(), http.clone(), auth)
}

#[tokio::test]
async fn reads_an_issue_with_its_comments_and_blockers() {
    let http = Arc::new(FakeHttp {
        answers: vec![("issue/CAMEL-42", raw_issue())],
        ..FakeHttp::default()
    });
    let issue = jira(&http, Auth::Anonymous)
        .issue("apache/camel", 42)
        .await
        .unwrap();
    assert_eq!(issue.number, 42);
    assert_eq!(issue.key.as_deref(), Some("CAMEL-42"));
    assert_eq!(issue.title, "camel-kafka: support headers");
    assert!(issue.open);
    assert_eq!(issue.labels, ["kafka"]);
    assert_eq!(issue.assignees, ["orpiske"]);
    assert_eq!(issue.milestone.as_deref(), Some("4.10.0"));
    assert_eq!(issue.blocked_by, [40, 41]);
    assert!(issue.body.starts_with("Please add it."));
    assert!(issue.body.contains("**Claus** (2026-11-02"));
    assert!(issue.body.contains("Use the existing mapper."));
    let (url, auth) = http.gets.lock().unwrap()[0].clone();
    assert!(url.starts_with("https://issues.example.org/jira/rest/api/2/issue/CAMEL-42?fields="));
    assert_eq!(auth, Auth::Anonymous);
}

#[tokio::test]
async fn searches_with_jql() {
    let http = Arc::new(FakeHttp {
        answers: vec![("search?jql=", json!({ "issues": [raw_issue()] }))],
        ..FakeHttp::default()
    });
    let mut config = settings();
    config.jql = Some("assignee = currentUser()".into());
    let source = Jira::with_auth(config, http.clone(), Auth::Anonymous);
    let selector = Selector {
        labels: vec!["amaru".into()],
        assignee: None,
    };
    let found = source.eligible("apache/camel", &selector).await.unwrap();
    assert_eq!(found[0].number, 42);
    source.milestone("apache/camel", "4.10.0").await.unwrap();
    let gets = http.gets.lock().unwrap();
    assert!(gets[0].0.contains(&encode(
        "project = \"CAMEL\" AND statusCategory != Done AND (labels in (\"amaru\") OR (assignee = currentUser())) ORDER BY created ASC"
    )));
    assert!(gets[1].0.contains(&encode("(fixVersion = \"4.10.0\")")));
}

#[tokio::test]
async fn picks_up_nothing_without_a_selector() {
    let http = Arc::new(FakeHttp::default());
    let found = jira(&http, Auth::Anonymous)
        .eligible("apache/camel", &Selector::default())
        .await
        .unwrap();
    assert!(found.is_empty());
    assert!(http.gets.lock().unwrap().is_empty());
}

#[tokio::test]
async fn comments_only_with_a_token_and_never_fails_on_it() {
    let http = Arc::new(FakeHttp::default());
    jira(&http, Auth::Anonymous)
        .comment("apache/camel", 42, "hi")
        .await
        .unwrap();
    assert!(http.posts.lock().unwrap().is_empty());
    let token = Auth::Bearer("secret".into());
    jira(&http, token)
        .comment(
            "apache/camel",
            42,
            "Amaru stopped:\n\n> it broke\n\nRun `amaru unblock 3`.",
        )
        .await
        .unwrap();
    let (url, body) = http.posts.lock().unwrap()[0].clone();
    assert_eq!(
        url,
        "https://issues.example.org/jira/rest/api/2/issue/CAMEL-42/comment"
    );
    assert_eq!(
        body["body"],
        "Amaru stopped:\n\n{quote}\nit broke\n{quote}\n\nRun {{amaru unblock 3}}."
    );
}

#[tokio::test]
async fn a_refused_comment_does_not_stop_the_work() {
    let refusing = Arc::new(FakeHttp {
        refuse_posts: true,
        ..FakeHttp::default()
    });
    let source = jira(&refusing, Auth::Bearer("secret".into()));
    assert!(source.comment("apache/camel", 42, "hi").await.is_ok());
}

#[tokio::test]
async fn does_not_label_jira_issues() {
    let http = Arc::new(FakeHttp::default());
    let source = jira(&http, Auth::Bearer("t".into()));
    assert!(
        source
            .add_label("apache/camel", 42, "amaru:working")
            .await
            .is_err()
    );
    assert!(
        source
            .remove_label("apache/camel", 42, "amaru:working")
            .await
            .is_ok()
    );
}

#[test]
fn writes_curl_config_without_the_token_on_the_command_line() {
    let auth = Auth::Basic {
        user: "bot".into(),
        token: "t\"k".into(),
    };
    let config = curl_config("https://x/a?b=1", &auth, Some(&json!({ "body": "a\nb" })));
    assert!(config.contains("url = \"https://x/a?b=1\"\n"));
    assert!(config.contains("user = \"bot:t\\\"k\"\n"));
    assert!(config.contains(r#"data-binary = "{\"body\":\"a\\nb\"}""#));
    assert_eq!(format!("{auth:?}"), "Basic");
    let bearer = curl_config("https://x", &Auth::Bearer("tok".into()), None);
    assert!(bearer.contains("header = \"Authorization: Bearer tok\""));
}

#[test]
fn converts_markdown_to_wiki_markup() {
    assert_eq!(to_wiki("on branch `amaru/x`."), "on branch {{amaru/x}}.");
    assert_eq!(to_wiki("odd ` tick"), "odd ` tick");
    assert_eq!(to_wiki("> a\n>\n> b"), "{quote}\na\n\nb\n{quote}");
}
