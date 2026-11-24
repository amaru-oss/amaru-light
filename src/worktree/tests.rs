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
        .create(&target, "amaru/1-fix", "origin/main")
        .await
        .unwrap();
    assert_eq!(ws.ahead(&wt, "origin/main").await.unwrap(), 0);
    std::fs::write(wt.join("fix.txt"), "fixed\n").unwrap();
    std::fs::create_dir_all(wt.join(".amaru")).unwrap();
    std::fs::write(wt.join(".amaru/result.json"), "{}").unwrap();
    assert!(ws.commit_leftovers(&wt, "fix").await.unwrap());
    assert!(!ws.commit_leftovers(&wt, "again").await.unwrap());
    assert_eq!(ws.ahead(&wt, "origin/main").await.unwrap(), 1);
    let (log, stat) = ws.summary(&wt, "origin/main").await.unwrap();
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
        .create(&target, "amaru/1-fix", "origin/main")
        .await
        .unwrap();
    std::fs::write(wt.join("fix.txt"), "fixed\n").unwrap();
    ws.commit_leftovers(&wt, "fix").await.unwrap();
    let stacked = ws
        .create(&target, "amaru/2-next", "amaru/1-fix")
        .await
        .unwrap();
    assert_eq!(ws.ahead(&stacked, "origin/main").await.unwrap(), 1);
    assert_eq!(
        ws.create(&target, "amaru/1-fix", "origin/main")
            .await
            .unwrap(),
        wt
    );
    ws.remove("me/app", &wt).await.unwrap();
    assert!(!wt.exists());
    // Recreating the worktree keeps the branch's commits.
    let again = ws
        .create(&target, "amaru/1-fix", "origin/main")
        .await
        .unwrap();
    assert_eq!(ws.ahead(&again, "origin/main").await.unwrap(), 1);
}
