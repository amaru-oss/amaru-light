//! Freeing disk: deleting the worktrees no unfinished ticket uses, with the
//! build output in them. The daemon does it on every poll; `amaru clean`
//! does it by hand.

use crate::{
    lifecycle::Services,
    ticket::{State, Ticket},
    worktree::disk_usage,
};
use anyhow::Result;
use std::{collections::HashSet, path::PathBuf};

/// What to delete besides the worktrees of finished tickets.
#[derive(Clone, Debug, Default)]
pub struct Sweep {
    /// Tickets with a job running; their worktrees stay whatever their state.
    pub running: Vec<i64>,
    /// Also delete the worktrees of blocked tickets. Their branches stay, and
    /// unblocking recreates the worktree from the branch.
    pub blocked: bool,
    /// Report what would be deleted, and delete nothing.
    pub dry_run: bool,
}

/// One worktree deleted, or that a dry run would delete, or that could not
/// be deleted.
#[derive(Clone, Debug)]
pub struct Removed {
    pub repo: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// Why it could not be deleted.
    pub error: Option<String>,
}

/// Lowercase, because repository names on GitHub are not case-sensitive and
/// a ticket may spell its repository differently from the project.
fn key(path: &std::path::Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// The worktree paths a ticket may be using: the one it recorded, and the
/// one its branch gets.
fn paths(svc: &Services, ticket: &Ticket) -> Vec<String> {
    let mut keys: Vec<String> = ticket.worktree.iter().map(|p| key(p)).collect();
    if let Some(branch) = &ticket.branch {
        let project = svc.settings.project(&ticket.repo);
        let repos = [Some(ticket.repo.as_str()), project.map(|p| p.repo.as_str())];
        for repo in repos.into_iter().flatten() {
            keys.push(key(&svc.workspace.path_of(repo, branch)));
        }
    }
    keys
}

/// Delete every worktree that no unfinished ticket uses: those of tickets
/// that are done or cancelled, and those whose ticket no longer exists. A
/// worktree that cannot be deleted is reported, and the others still go.
pub async fn sweep(svc: &Services, options: &Sweep) -> Result<Vec<Removed>> {
    // List the directories before reading the tickets: a ticket records its
    // branch before its worktree is created, so a worktree that exists now
    // belongs to a ticket the read below sees.
    let found = svc.workspace.worktrees()?;
    let keep = in_use(svc, options)?;
    let mut removed = Vec::new();
    for (repo, path) in found {
        if keep.contains(&key(&path)) {
            continue;
        }
        let measured = path.clone();
        let bytes = tokio::task::spawn_blocking(move || disk_usage(&measured)).await?;
        let error = if options.dry_run {
            None
        } else {
            let result = svc.workspace.remove(&repo, &path).await;
            result.err().map(|e| format!("{e:#}"))
        };
        removed.push(Removed {
            repo,
            path,
            bytes,
            error,
        });
    }
    Ok(removed)
}

/// The worktree paths to keep. With `options.blocked`, blocked tickets give
/// theirs up, and forget them.
fn in_use(svc: &Services, options: &Sweep) -> Result<HashSet<String>> {
    let mut keep = HashSet::new();
    for mut ticket in svc.store.unfinished()? {
        let running = options.running.contains(&ticket.id);
        if options.blocked && ticket.state == State::Blocked && !running {
            if !options.dry_run && ticket.worktree.take().is_some() {
                svc.store.save(&ticket)?;
            }
            continue;
        }
        keep.extend(paths(svc, &ticket));
    }
    for id in &options.running {
        if let Some(ticket) = svc.store.get(*id)? {
            keep.extend(paths(svc, &ticket));
        }
    }
    Ok(keep)
}

/// Bytes as a short human-readable size.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    #[allow(clippy::cast_precision_loss)]
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lifecycle::BLOCKED_LABEL, testing::Fake};

    async fn worked(fake: &Fake, number: u64) -> Ticket {
        fake.drive(fake.claim(number).await).await
    }

    fn has_branch(fake: &Fake, pattern: &str) -> bool {
        let out = std::process::Command::new("git")
            .args(["branch", "--list", pattern])
            .current_dir(fake.svc.workspace.mirror("me/app"))
            .output()
            .unwrap();
        !String::from_utf8_lossy(&out.stdout).trim().is_empty()
    }

    #[tokio::test]
    async fn deletes_only_what_no_unfinished_ticket_uses() {
        let fake = Fake::new("").await;
        let kept = worked(&fake, 1).await.worktree.unwrap();
        let mut cancelled = worked(&fake, 2).await;
        let gone = cancelled.worktree.clone().unwrap();
        std::fs::create_dir_all(gone.join("target/debug")).unwrap();
        std::fs::write(gone.join("target/debug/big"), vec![0u8; 4096]).unwrap();
        cancelled.state = State::Cancelled;
        fake.svc.store.save(&cancelled).unwrap();
        // A worktree whose ticket was deleted from the database.
        let stray = fake.svc.workspace.path_of("me/app", "amaru/9-stray");
        std::fs::create_dir_all(&stray).unwrap();

        let dry = Sweep {
            dry_run: true,
            ..Sweep::default()
        };
        let listed = sweep(&fake.svc, &dry).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(gone.exists() && stray.exists());
        assert!(listed.iter().any(|r| r.path == gone && r.bytes >= 4096));

        let removed = sweep(&fake.svc, &Sweep::default()).await.unwrap();
        assert!(removed.iter().all(|r| r.error.is_none()));
        assert_eq!(removed.len(), 2);
        assert!(kept.exists() && !gone.exists() && !stray.exists());
        let again = sweep(&fake.svc, &Sweep::default()).await.unwrap();
        assert!(again.is_empty());
        assert!(has_branch(&fake, "amaru/2-*"));
    }

    #[tokio::test]
    async fn keeps_running_tickets_and_blocked_ones_unless_asked() {
        let fake = Fake::new("").await;
        let mut blocked = worked(&fake, 1).await;
        let mut stopping = worked(&fake, 2).await;
        blocked.state = State::Blocked;
        blocked.labels.push(BLOCKED_LABEL.into());
        fake.svc.store.save(&blocked).unwrap();
        // Cancelled while its job still runs.
        stopping.state = State::Cancelled;
        fake.svc.store.save(&stopping).unwrap();
        let running = Sweep {
            running: vec![stopping.id],
            ..Sweep::default()
        };
        assert!(sweep(&fake.svc, &running).await.unwrap().is_empty());

        let all = Sweep {
            running: vec![stopping.id],
            blocked: true,
            ..Sweep::default()
        };
        let removed = sweep(&fake.svc, &all).await.unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!blocked.worktree.clone().unwrap().exists());
        let stored = fake.svc.store.get(blocked.id).unwrap().unwrap();
        assert_eq!(stored.worktree, None);
        assert_eq!(stored.state, State::Blocked);
    }

    #[test]
    fn prints_sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
