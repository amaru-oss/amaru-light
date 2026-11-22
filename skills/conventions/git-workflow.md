# Git workflow

- Work only inside the working directory you were started in (a git worktree of the project).
- Inspect `git status` before changes. Do not overwrite unrelated changes.
- In a normal job: do not commit or push.
- In loop work items (the prompt says so): commit your finished work on the CURRENT branch following the
  repository's AGENTS.md (English conventional commits, one logical change per commit, no secrets).
  Never push, merge, switch branches, force anything, or alter issues/PRs; the orchestrator does that.
- Report branch, modified files, commits, and anything not validated.
