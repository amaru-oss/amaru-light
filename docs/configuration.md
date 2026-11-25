# Configuration reference

Amaru Light reads two kinds of YAML files:

- One **daemon file**, usually `amaru.yaml`, with the machine-wide settings
  and the list of project files. `amaru` reads it from `--config` (`-c`),
  `amaru.yaml` in the current directory by default.
- One **project file** per repository, listed under `projects:` in the
  daemon file.

Both are checked strictly. An unknown or misspelled key is an error at
startup, not a silent default. A file from Amaru Light v1 therefore fails
to load; see [Migrating from v1](migrating-from-v1.md).

Relative paths in the daemon file (`state_dir`, `projects`) resolve against
the directory of the daemon file, not the current directory.

Durations are written as a number and a unit: `30s`, `45m`, `2h`, `7d`.

Run `amaru -c amaru.yaml doctor` after every change. It loads both files and
checks Git, `gh auth status` and each agent CLI.

## The daemon file

```yaml
state_dir: data
poll_interval: 60s
max_parallel: 2
gh: gh
bot_login: my-bot

agents:
  codex:
    driver: codex
    command: codex
    args: [--dangerously-bypass-approvals-and-sandbox]

projects:
  - project.yaml
```

| Key | Default | Meaning |
| --- | --- | --- |
| `state_dir` | `data` | The database (`amaru.db`), the repository clones and the ticket worktrees. In a container, put it on a volume. |
| `poll_interval` | `60s` | How often the daemon looks for new issues, ticket changes and pull request activity. |
| `max_parallel` | `2` | Tickets that hold a worker slot at once: `1` or `2`. A ticket waiting for review does not hold a slot. |
| `gh` | `gh` | The GitHub CLI executable. It reads the bot's token from `GH_TOKEN`. |
| `bot_login` | none | The bot's GitHub login. Its own comments and reviews are never treated as feedback. |
| `agents` | required | The agent CLIs, by a name of your choice. Projects refer to that name. |
| `projects` | required | The project files. |

### Agents

| Key | Default | Meaning |
| --- | --- | --- |
| `driver` | required | `claude` (Claude Code) or `codex` (Codex CLI): which output format to read. |
| `command` | required | The executable, on `PATH` or as an absolute path. |
| `args` | `[]` | Extra arguments, added to every call. Use them for the permission mode. |
| `model` | none | Passed as `--model`. Leave it out to use the CLI's default. |
| `env` | `{}` | Extra environment variables for the agent process. |

The daemon adds the arguments it needs itself; do not repeat them in
`args`:

- Claude Code: `claude -p --output-format stream-json --verbose [--model m] [args] [--resume <session>]`, with the prompt on stdin.
- Codex: `codex exec --json [--model m] [args] [resume <session>] -`, with the prompt on stdin.

The agent runs unattended, so it must not stop to ask for permission. It
also has to run the project's build, which often needs the network:

```yaml
agents:
  claude:
    driver: claude
    command: claude
    model: sonnet
    args: [--permission-mode, bypassPermissions]
  codex:
    driver: codex
    command: codex
    # The container is the sandbox. --full-auto keeps Codex's own sandbox
    # instead, which blocks network access by default.
    args: [--dangerously-bypass-approvals-and-sandbox]
```

Sign in to each CLI once as the user that runs the daemon (`claude` then
`/login`, or `codex login --device-auth`). The login lives in that user's
home directory.

## The project file

```yaml
repo: owner/name
base_branch: main
fork: my-bot/name
branch_prefix: amaru/

intake:
  labels: [amaru]
  assignee: my-bot
  skip_labels: [amaru:hold, needs-human]
  priority_labels: [priority:high]

agent: codex
step_timeout: 45m

checks:
  - name: check
    run: make check
    timeout: 30m
max_fix_rounds: 3

review:
  self: off
  respond: true
  reviewers: [owner]
  max_rounds: 5
  fix_ci: true

workspace:
  keep_blocked: 7d
```

| Key | Default | Meaning |
| --- | --- | --- |
| `repo` | required | `owner/name` of the repository whose issues the bot works on and where pull requests are opened. No trailing slash. |
| `base_branch` | `main` | The branch that ticket branches start from and target. |
| `fork` | none | `owner/name` of the bot's fork. When set, branches are pushed there and pull requests come from `owner:branch`. When not set, branches are pushed to `repo` itself. See [GitHub access](github-access.md). |
| `branch_prefix` | `amaru/` | Ticket branches are `<prefix><issue>-<slug>`, for example `amaru/42-fix-the-export-button`. Include the separator, `/` or `-`, in the prefix. |
| `agent` | required | A name from `agents:` in the daemon file. |
| `step_timeout` | `45m` | The longest one agent turn may run. |
| `checks` | `[]` | Commands the daemon runs itself after each agent turn, in order, through `sh -c` in the ticket's worktree. |
| `max_fix_rounds` | `3` | How many times failing checks go back to the agent before the ticket blocks. |

### Intake

`intake` decides which issues `amaru serve` picks up by itself. It needs
`labels`, `assignee` or both: an issue that matches either is eligible.
Issues claimed by hand (`amaru claim`) or through a milestone
(`amaru drain`) ignore `labels` and `assignee`.

| Key | Default | Meaning |
| --- | --- | --- |
| `labels` | `[]` | An open issue with any of these labels is eligible. |
| `assignee` | none | An open issue assigned to this login is eligible. GitHub only lets you assign people who have access to the repository. |
| `skip_labels` | `[amaru:hold, needs-human]` | Issues with any of these labels are never started. Setting this list replaces the default, so keep `amaru:hold` in it if you want it. |
| `priority_labels` | `[]` | Issues with these labels go first, in the order listed. Then the milestone due date decides, then age. |

An issue whose body says `Depends on #12` or `Blocked by #12` starts only
once #12 has a pull request, on a branch stacked on #12's branch.

### Checks

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | required | A short name for logs and events. |
| `run` | required | The shell command. Shell operators such as `&&` work. |
| `timeout` | `30m` | The check fails when it runs longer. |

### Review

| Key | Default | Meaning |
| --- | --- | --- |
| `self` | `off` | Self-review before the pull request opens: `off`, `same_agent` (the ticket's own session reviews the diff) or `other_agent` (a fresh session of `reviewer_agent`). |
| `reviewer_agent` | none | Needed when `self` is `other_agent`. |
| `respond` | `true` | Act on review comments and change requests on the pull request. |
| `reviewers` | `[]` | Only feedback from these logins reaches the agent. With an empty list, no human feedback does. |
| `max_rounds` | `5` | Review rounds before the ticket blocks. |
| `fix_ci` | `true` | Treat failing CI check runs on the pull request as feedback. |

### Workspace

| Key | Default | Meaning |
| --- | --- | --- |
| `keep_blocked` | `7d` | How long the worktree of a blocked ticket is kept: a duration, `0` or `forever`. The branch is kept either way. Unblocking recreates the worktree from it. |

## Prompts

The prompts are compiled into the binary. To change one, copy it from
[`prompts/`](../prompts) into a `prompts/` directory next to the daemon
file, keeping its file name, and edit it there. Prompts you don't copy keep
their built-in text.
