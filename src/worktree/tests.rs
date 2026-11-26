use super::*;

const ID: [&str; 4] = ["-c", "user.name=test", "-c", "user.email=test@example.com"];

async fn run(cwd: &Path, args: &[&str]) -> String {
    git(cwd, args).await.unwrap()
}

/// A bare upstream `me/app` with one commit on main, and an empty fork `bot/app`.
async fn remotes(root: &Path) -> String {
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    run(&seed, &["init", "--quiet", "-b", "main"]).await;
    std::fs::write(seed.join("README"), "hi\n").unwrap();
    run(&seed, &["add", "README"]).await;
    run(
        &seed,
        &[&ID[..], &["commit", "--quiet", "-m", "init"]].concat(),
    )
    .await;
    for (repo, source) in [("me/app.git", "seed"), ("bot/app.git", "seed")] {
        let dest = root.join("remotes").join(repo);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let (src, dst) = (root.join(source), dest.to_string_lossy().into_owned());
        run(
            root,
            &["clone", "--quiet", "--bare", &src.to_string_lossy(), &dst],
        )
        .await;
    }
    format!("file://{}/remotes/", root.display())
}

struct Fixture {
    dir: tempfile::TempDir,
    ws: Workspace,
    target: Target,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = remotes(dir.path()).await;
    let ws = Workspace::new(&dir.path().join("state")).with_url_base(&base);
    let target = Target {
        upstream: "me/app".into(),
        fork: Some("bot/app".into()),
    };
    let mirror = ws.sync(&target).await.unwrap();
    run(&mirror, &["config", "user.name", "amaru"]).await;
    run(&mirror, &["config", "user.email", "amaru@example.com"]).await;
    Fixture { dir, ws, target }
}

async fn branches(remote: &Path) -> String {
    run(remote, &["branch", "--list", "amaru/*"]).await
}

#[tokio::test]
async fn commits_leftovers_and_pushes_to_the_fork() {
    let Fixture { dir, ws, target } = fixture().await;
    let wt = ws
        .create(&target, "amaru/1-fix", "upstream/main")
        .await
        .unwrap();
    assert_eq!(ws.ahead(&wt, "upstream/main").await.unwrap(), 0);
    std::fs::write(wt.join("fix.txt"), "fixed\n").unwrap();
    std::fs::create_dir_all(wt.join(".amaru")).unwrap();
    std::fs::write(wt.join(".amaru/result.json"), "{}").unwrap();
    assert!(ws.commit_leftovers(&wt, "fix").await.unwrap());
    assert!(!ws.commit_leftovers(&wt, "again").await.unwrap());
    assert_eq!(ws.ahead(&wt, "upstream/main").await.unwrap(), 1);
    let (log, stat) = ws.summary(&wt, "upstream/main").await.unwrap();
    assert_eq!(log.trim(), "- fix");
    assert!(stat.contains("fix.txt") && !stat.contains(".amaru"));
    ws.push(&wt, &target, "amaru/1-fix").await.unwrap();
    let remotes = dir.path().join("remotes");
    assert!(
        branches(&remotes.join("bot/app.git"))
            .await
            .contains("amaru/1-fix")
    );
    assert!(
        branches(&remotes.join("me/app.git"))
            .await
            .trim()
            .is_empty()
    );
}

#[tokio::test]
async fn stacks_reuses_and_removes_worktrees() {
    let Fixture {
        dir: _dir,
        ws,
        target,
    } = fixture().await;
    let wt = ws
        .create(&target, "amaru/1-fix", "upstream/main")
        .await
        .unwrap();
    std::fs::write(wt.join("fix.txt"), "fixed\n").unwrap();
    ws.commit_leftovers(&wt, "fix").await.unwrap();
    let stacked = ws
        .create(&target, "amaru/2-next", "amaru/1-fix")
        .await
        .unwrap();
    assert_eq!(ws.ahead(&stacked, "upstream/main").await.unwrap(), 1);
    assert_eq!(
        ws.create(&target, "amaru/1-fix", "upstream/main")
            .await
            .unwrap(),
        wt
    );
    ws.remove("me/app", &wt).await.unwrap();
    assert!(!wt.exists());
    // Recreating the worktree keeps the branch's commits.
    let again = ws
        .create(&target, "amaru/1-fix", "upstream/main")
        .await
        .unwrap();
    assert_eq!(ws.ahead(&again, "upstream/main").await.unwrap(), 1);
}

async fn url(dir: &Path, args: &[&str]) -> String {
    run(dir, &[&["remote", "get-url"][..], args].concat())
        .await
        .trim()
        .to_owned()
}

/// Assert that `dir` fetches and pushes `origin` at `origin` and `upstream`
/// at `me/app`, under the remotes of `root`.
async fn assert_remotes(root: &Path, dir: &Path, origin: &str) {
    let base = format!("file://{}/remotes/", root.display());
    for (name, repo) in [("origin", origin), ("upstream", "me/app")] {
        let want = format!("{base}{repo}.git");
        assert_eq!(url(dir, &[name]).await, want, "{name}");
        assert_eq!(url(dir, &["--push", name]).await, want, "{name} push");
    }
    let names = run(dir, &["remote"]).await;
    assert_eq!(
        names.split_whitespace().collect::<Vec<_>>(),
        ["origin", "upstream"]
    );
}

#[tokio::test]
async fn origin_is_the_fork_and_upstream_the_repository() {
    let Fixture { dir, ws, target } = fixture().await;
    let wt = ws
        .create(&target, "amaru/1-fix", "upstream/main")
        .await
        .unwrap();
    assert_remotes(dir.path(), &ws.mirror("me/app"), "bot/app").await;
    assert_remotes(dir.path(), &wt, "bot/app").await;
}

#[tokio::test]
async fn puts_back_remotes_the_agent_changed() {
    let Fixture { dir, ws, target } = fixture().await;
    let wt = ws
        .create(&target, "amaru/1-fix", "upstream/main")
        .await
        .unwrap();
    let elsewhere = "file:///nowhere.git";
    run(&wt, &["remote", "set-url", "origin", elsewhere]).await;
    run(&wt, &["remote", "set-url", "--push", "upstream", elsewhere]).await;
    run(&wt, &["remote", "add", "fork", elsewhere]).await;
    ws.ensure_remotes(&target, Some(&wt)).await.unwrap();
    assert_remotes(dir.path(), &wt, "bot/app").await;
    // Running it again changes nothing.
    ws.ensure_remotes(&target, Some(&wt)).await.unwrap();
    assert_remotes(dir.path(), &wt, "bot/app").await;
    // The push checks the remotes too, so a change right before it is undone.
    std::fs::write(wt.join("fix.txt"), "fixed\n").unwrap();
    ws.commit_leftovers(&wt, "fix").await.unwrap();
    run(&wt, &["remote", "set-url", "--push", "origin", elsewhere]).await;
    ws.push(&wt, &target, "amaru/1-fix").await.unwrap();
    let remotes = dir.path().join("remotes");
    assert!(
        branches(&remotes.join("bot/app.git"))
            .await
            .contains("amaru/1-fix")
    );
    assert!(
        branches(&remotes.join("me/app.git"))
            .await
            .trim()
            .is_empty()
    );
}

#[tokio::test]
async fn moves_an_old_clone_to_the_origin_fork_layout() {
    let dir = tempfile::tempdir().unwrap();
    let base = remotes(dir.path()).await;
    let ws = Workspace::new(&dir.path().join("state")).with_url_base(&base);
    // The layout of earlier versions: origin = upstream, fork = the fork.
    let mirror = ws.mirror("me/app");
    std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
    let (upstream, fork) = (format!("{base}me/app.git"), format!("{base}bot/app.git"));
    let dest = mirror.to_string_lossy().into_owned();
    run(dir.path(), &["clone", "--quiet", &upstream, &dest]).await;
    run(&mirror, &["remote", "add", "fork", &fork]).await;
    let target = Target {
        upstream: "me/app".into(),
        fork: Some("bot/app".into()),
    };
    ws.sync(&target).await.unwrap();
    assert_remotes(dir.path(), &mirror, "bot/app").await;
    run(&mirror, &["rev-parse", "--verify", "upstream/main"]).await;
}

#[tokio::test]
async fn without_a_fork_both_remotes_are_the_repository() {
    let dir = tempfile::tempdir().unwrap();
    let base = remotes(dir.path()).await;
    let ws = Workspace::new(&dir.path().join("state")).with_url_base(&base);
    let target = Target {
        upstream: "me/app".into(),
        fork: None,
    };
    let mirror = ws.sync(&target).await.unwrap();
    assert_remotes(dir.path(), &mirror, "me/app").await;
    run(&mirror, &["rev-parse", "--verify", "upstream/main"]).await;
}
