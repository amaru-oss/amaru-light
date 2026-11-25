# Troubleshooting

In the commands below, `amaru` stands for the binary with your daemon file,
for example `amaru -c amaru.yaml`, or in a container
`podman exec <container> /app/bin/amaru -c /config/amaru.yaml`.

## Seeing what the daemon does

- **The log.** The daemon writes one line per state change, repository
  sync, agent turn, agent command and check result to stderr
  (`podman logs -f <container>` in a container):

  ```
  owner/app#42: QUEUED -> PREPARING
  owner/app#42: syncing owner/app
  owner/app#42: PREPARING -> IMPLEMENTING
  owner/app#42: codex turn started (new session)
  owner/app#42: codex runs /bin/bash -lc 'cargo test'
  owner/app#42: codex turn ended (Code(0))
  owner/app#42: running the checks
  ```

  An agent turn can take many minutes. Each command the agent runs appears
  as a line, cut to its first line.

- **The tickets.** `amaru status` lists recent tickets with their ID, state,
  issue, pull request and the reason a ticket is blocked.
  `amaru status --ticket <id>` shows every event of one ticket, including
  the agent's messages and its full commands.

- **The issue.** The bot comments when it starts an issue, with the branch
  name, and when it needs you, with the reason.

Ticket IDs are the daemon's own numbers, not issue numbers. Claiming the
same issue again after a cancel makes a new ticket with a new ID.

## Startup and settings

**`unknown field ...` when the daemon starts.** The settings are checked
strictly. The message names the key and the file. A v1 configuration
always fails this way; see [Migrating from v1](migrating-from-v1.md).

**`intake needs labels, an assignee, or both`.** Every project needs at
least one of `intake.labels` and `intake.assignee`, even if you only use
`claim` and `drain`.

**`repo 'owner/name/' must be owner/name`.** Drop the trailing slash.

**The project uses an agent that is not defined.** `agent:` in the project
file must be a name under `agents:` in the daemon file.

**Relative paths point to the wrong place.** `state_dir` and `projects` are
relative to the daemon file, not to the current directory.

## Claiming work

**`owner/name#42 already has an unfinished ticket`.** The issue already has
a ticket. Look it up with `amaru status`:

- `BLOCKED`: run `amaru unblock <id>`. It continues from the step where it
  stopped.
- `QUEUED` or another active state: nothing to do. The running daemon picks
  it up within one poll interval.
- To start over: `amaru cancel <id>`, then claim again. The new ticket
  reuses the branch if it exists.

**`zsh: no matches found: owner/name#42`.** Quote the reference:
`amaru claim 'owner/name#42'`.

**`claim` queued the ticket but nothing happens.** `claim` only queues. A
running `amaru serve` starts it on its next poll; without one, use
`amaru claim <issue> --now`, which starts a daemon for manual tickets in
that terminal.

**The daemon ignores an issue.** It skips closed issues, issues with a skip
label, issues already labelled `amaru:working` or `amaru:blocked`, and
issues that depend on an issue without a pull request yet. In autonomous
mode the issue must also match `intake.labels` or `intake.assignee`.

**Two daemons on the same state.** Run one daemon per `state_dir`. To
drain a milestone, stop `serve` first, run `drain`, then start `serve`
again. Both use the same database, so tickets carry over.

## GitHub

**`Must have admin rights to Repository. (HTTP 403)` when adding a label.**
The bot cannot label issues of the upstream repository, which is usual for
a bot that pushes to a fork. The daemon logs it and goes on without
labels. Blocked tickets then resume with `amaru unblock <id>` instead of
by removing the label. See [GitHub access](github-access.md).

**The clone works but the push fails with an authentication error.** Git
does not read `GH_TOKEN` by itself. Run `gh auth setup-git` once, or set
the credential helper in the image; see
[Git credentials](github-access.md#git-credentials).

**The pull request cannot be created.** With a fork, check that `fork`
names a fork the bot owns, and that the token can act on the upstream
repository: a fine-grained token cannot for a repository it was not
granted. Use a classic token with `public_repo`.

**Assigning the bot does nothing.** GitHub only lets you assign people with
access to the repository. Use `intake.labels` for a bot without access.

**Review comments are ignored.** Only feedback from `review.reviewers`
reaches the agent, and none from `bot_login`. Check the spelling of the
logins.

## Agents

**The agent stops to ask for permission, or cannot run the build.** Give
it the arguments for unattended work in `args`:
`[--permission-mode, bypassPermissions]` for Claude Code,
`[--dangerously-bypass-approvals-and-sandbox]` for Codex in a container.
Codex's `--full-auto` keeps its own sandbox, which blocks the network that
most builds need.

**`doctor` fails for an agent.** The `command` must start as the user that
runs the daemon, from that user's `PATH`. Sign in to the CLI as that user:
`claude` then `/login`, or `codex login --device-auth`.

**A ticket blocks with `the agent did not report its result`.** The agent
ended its turn without writing the result file, `.amaru/result.json`, even
after a reminder. Read its last messages with `amaru status --ticket <id>`,
then `amaru unblock <id>`.

**A ticket blocks with `the agent did not finish within 2700s`.** The turn hit
`step_timeout`. Raise it in the project file for large issues, then
`amaru unblock <id>`: the next turn resumes the same session.

**A ticket blocks with `the agent failed: ...`.** The CLI exited with an
error, for example a usage limit or an expired login. The reason holds the
end of its output.

## Containers

**`/home/mini-amaru/...` is empty: where is the code?** Under `state_dir`:
`<state_dir>/repos/<owner>/<name>/mirror` is the clone and
`<state_dir>/repos/<owner>/<name>/worktrees/<branch>` holds one worktree
per ticket, with `/` in the branch name replaced by `-`. The clone is made
when the first ticket starts preparing.

**An unused anonymous volume appears for `/data`.** The base image declares
`/data` as a volume for `state_dir`. If you keep the state in the home
volume instead, Podman still creates an empty volume for `/data`;
`podman rm -v` removes it with the container.

**The settings in the image are out of date.** If the image copies
`amaru.yaml` and the project file at build time, rebuild it after each
change, or mount the directory at `/config` instead.
