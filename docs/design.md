# Amaru Light v2: design

Amaru Light v2 is a long-running personal daemon. It takes GitHub tickets,
works each one in a single agent CLI session, opens a pull request and keeps
fixing it from review feedback until a human merges it. It never merges.

## Goals and non-goals

Goals:

- Full autonomy for one person, with as few moving parts as possible.
- About as fast and token-efficient as using Claude Code or Codex by hand.
- Reliable: survives restarts, retries transient failures, and explains every
  stop on the ticket.
- Flexible: several ways to claim work, per-project pipelines, pluggable ticket
  sources and agent CLIs.
- Runs unattended for days on a laptop, a home server or a cloud VM.

Non-goals: team features (multi-tenancy, RBAC), auto-merge, agent SDKs, a
general workflow engine.

## Why v1 falls short

| Problem | Effect |
| --- | --- |
| Every step is a one-shot CLI call; success is exit code 0 | No session reuse, a retry starts from nothing |
| Planner, brain, PR text and reviewer scrape JSON from prose | Frequent BLOCKED and fallback outcomes |
| Up to 30 planned tasks, each a fresh agent with 5 summaries of context | 6 to 10 cold starts per issue, no replanning |
| One check run and one fix attempt, review of a truncated diff | Gives up early |
| Workflow hard-coded in `delivery.rs`, untyped configuration | Rigid; typos silently fall back to defaults |
| One worker thread, resume creates a new job | No parallelism, resume loses work |

## Principles

1. **One agent session per ticket.** Implementation, check fixes and review
   fixes all resume the same CLI session.
2. **Deterministic orchestrator.** Rust owns tickets, worktrees, branches,
   checks, pushes, PRs and labels. The agent writes code. No LLM picks work.
3. **Structured results.** The agent writes `.amaru/result.json`, validated
   against a schema. Nothing is scraped from prose.
4. **Verification gates the work.** Checks are real commands; failures go back
   into the session, up to a budget.
5. **Effort scales with the ticket.** Small tickets skip planning.
6. **Every stop is explained on the ticket.**
7. **Typed configuration** (`deny_unknown_fields`), prompt templates as files.

## Claiming work

All modes feed one queue; after intake every ticket follows the same lifecycle.

| Mode | Trigger | Ends when |
| --- | --- | --- |
| Manual | `amaru claim owner/repo#42`, or the UI | That ticket is done |
| Milestone | `amaru drain --milestone M5` | Every open issue has a PR or is blocked |
| Autonomous | Issues with a label and/or assigned to the bot | Never (daemon) |

- Claiming adds the `amaru:working` label and a comment, so a ticket is never
  claimed twice.
- Issues labelled with a skip label (default `amaru:hold`, `needs-human`) are
  ignored.
- Order: priority labels, then milestone due date, then oldest first.
- An issue that depends on another issue's unmerged PR starts on a branch
  stacked on that PR's branch.
- At most 2 tickets are in progress at once (configurable, default 2).
- Removing the label or unassigning the bot cancels at the next safe point.

## Ticket lifecycle

```text
QUEUED -> PREPARING -> IMPLEMENTING -> CHECKING -> [SELF_REVIEW] -> PUBLISHING
                            ^              |
                            +-- fails -----+
PUBLISHING -> AWAITING_REVIEW -> DONE (a human merged the PR)
                  |      ^
                  v      | push and reply
              ADDRESSING_REVIEW
Any state -> BLOCKED (reason posted on the issue) | CANCELLED
```

`IMPLEMENTING`, check fixes and `ADDRESSING_REVIEW` share one resumable
session. A ticket in `AWAITING_REVIEW` does not use a worker slot.

## Components

| Component | Responsibility | First implementation |
| --- | --- | --- |
| `Scheduler` | Watchers, queue, worker slots, state machines | — |
| `TicketSource` | List, claim, comment, label, dependencies | GitHub via `gh` |
| `Forge` | Push, open PRs, read reviews and checks, reply | GitHub via `gh` |
| `AgentDriver` | Start, resume, stream events, usage, cancel | Claude Code; Codex next |
| `Workspace` | Repository mirror, one worktree per ticket, retention | git worktree |
| `Checks` | Project commands with timeouts | shell |
| `Store` | Tickets, state, session ids, event log | SQLite |

The daemon acts as a separate bot GitHub account. Its token comes from the
`GH_TOKEN` environment variable, as in the container images.

## Agent sessions and the result contract

- Claude Code: `claude -p --output-format stream-json --verbose`, then
  `--resume <session-id>`.
- Codex: `codex exec --json`, then `codex exec resume <session-id>`.
- Each step has a wall-clock budget. Exhausting it blocks the ticket.
- When a session cannot be resumed, a fresh one starts with a context pack:
  issue, current diff, review threads.

At the end of every step the agent writes `.amaru/result.json` (git-ignored
through `.git/info/exclude`):

```json
{
  "status": "done | blocked | needs_clarification",
  "summary": "what changed and why",
  "complete": true,
  "question": "only for needs_clarification",
  "replies": [{"thread_id": "...", "body": "how it was addressed"}]
}
```

A missing or invalid file triggers one resume asking for it.

## Review

| Setting | Values | Default |
| --- | --- | --- |
| `review.self` | `off`, `same_agent`, `other_agent` | `off` |
| `review.respond` | `true`, `false` | `true` |
| `review.reviewers` | GitHub logins | the operator |
| `review.max_rounds` | number | 5 |
| `review.fix_ci` | `true`, `false` | `true` |

Only allowlisted reviewers' comments reach the agent. Each round produces one
push and a reply per addressed thread. The daemon never resolves threads.

## Runtime and safety

- One process; polling (default 60 s), no webhooks needed.
- Each state transition is stored before its side effect, and side effects are
  idempotent, so a restart resumes where it stopped.
- Transient errors retry with backoff and do not consume fix budgets.
- No merges. Pushes only to branches the daemon created.
- The worktree of a BLOCKED ticket is kept for `workspace.keep_blocked`
  (for example `7d`, `0`, `forever`).

## Implementation notes

- The crate follows the Wanaku lint configuration (`clippy.toml`,
  `[lints]` in `Cargo.toml`): no `unwrap`, `expect`, `panic` or `unsafe` in
  production code, short functions, and no `std::thread::spawn` or
  `std::thread::sleep`. The daemon is therefore built on tokio.
- Configuration lives in `src/settings.rs`; `examples/` holds a daemon file and
  a project file that the tests load.
