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

Relative paths in the daemon file (`state_dir`, `projects`, an agent's
`instructions`) resolve against the directory of the daemon file, not the
current directory.

Durations are written as a number and a unit: `30s`, `45m`, `2h`, `7d`.

Run `amaru -c amaru.yaml doctor` after every change. It loads both files and
checks Git, `gh auth status` and each agent CLI, including whether it is
signed in.

## The daemon file

```yaml
state_dir: data
poll_interval: 60s
max_parallel: 2
gh: gh
bot_login: my-bot
web:
  listen: 127.0.0.1:8765

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
| `web.enabled` | `true` | Serve the [web interface](web-ui.md) next to `amaru serve`. |
| `web.listen` | `127.0.0.1:8765` | The address and port of the web interface. Use `0.0.0.0:8765` in a container, and set `AMARU_WEB_TOKEN` when anyone else can reach the port. |
| `agents` | required | The agent CLIs, by a name of your choice. Projects refer to that name. |
| `projects` | required | The project files. |

### Agents

| Key | Default | Meaning |
| --- | --- | --- |
| `driver` | required | `claude` (Claude Code), `codex` (Codex CLI) or `bob` (IBM Bob Shell): which output format to read. |
| `command` | required | The executable, on `PATH` or as an absolute path. |
| `args` | `[]` | Extra arguments, added to every call. Use them for the permission mode. |
| `model` | none | Passed as `--model`. Leave it out to use the CLI's default. For Bob it is the model tier, `fast`, `premium` or `ultra`, set as `BOB_SHELL_MODEL_TIER`. |
| `env` | `{}` | Extra environment variables for the agent process. The daemon's credentials (`GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN`, `AMARU_WEB_TOKEN` and each Jira `token_env`) are removed from the agent's and the checks' environment; setting one here gives it back on purpose. |
| `login` | by driver | Arguments to `command` that sign the CLI in from the web page. Claude Code: `[auth, login]`. Codex: `[login, --device-auth]`. Bob: none. `[]` turns the Sign in button off. |
| `login_with_key` | by driver | Arguments to `command` that sign the CLI in with an API key read from stdin, for the web page's **Use an API key**. The page writes the key and closes stdin. Codex: `[login, --with-api-key]`. Claude Code and Bob: none, since they read `ANTHROPIC_API_KEY` and `BOB_API_KEY` (set them in `env` or the container). `[]` turns it off. |
| `login_status` | by driver | Arguments to `command` that tell whether the CLI is signed in; exit code 0 means yes. The daemon runs it every poll and takes no work for the projects that use an agent that is not signed in; `[]` turns the check off. Claude Code: `[auth, status, --text]`. Codex: `[login, status]`. Bob: none, so neither the page nor the daemon can tell. |
| `setup` | `[]` | Argument lists for `command`, run in order each time `amaru serve`, `drain` or `claim --now` starts, for example to install plugins. See [Setting the agent CLIs up](#setting-the-agent-clis-up). |
| `instructions` | none | A file, relative to the daemon file, copied at each start to the CLI's user-wide instructions: `~/.claude/CLAUDE.md` for Claude Code, `~/.codex/AGENTS.md` for Codex, `~/.bob/rules/amaru.md` for Bob (which reads `AGENTS.md` only from the repository, and every file in `~/.bob/rules` as a user-wide rule). |
| `install` | by driver | The command line, program first, that installs or updates the CLI from the web page's **Install** button: `[npm, install, --global, --no-audit, --no-fund, <package>]` with `@anthropic-ai/claude-code`, `@openai/codex` or `@ibm/bob`. npm installs into `~/.local` unless `NPM_CONFIG_PREFIX` is set. After a successful install the agent's `instructions` and `setup` run, as at a start. `[]` turns the button off. See [Installing agents](web-ui.md#installing-agents). |

The daemon adds the arguments it needs itself; do not repeat them in
`args`:

- Claude Code: `claude -p --output-format stream-json --verbose [--model m] [args] [--resume <session>]`, with the prompt on stdin.
- Codex: `codex exec --json [--model m] [args] [resume <session>] -`, with the prompt on stdin.
- Bob: `bob run --format stream-json --trust [--resume <task>] [args]`, with the prompt on stdin and the model tier in `BOB_SHELL_MODEL_TIER`.

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
  bob:
    driver: bob
    command: bob
    # fast, premium (Bob's default) or ultra.
    model: premium
    # Accepts the IBM license for the account that runs the daemon. Leave
    # it out if you accept it once yourself in `bob chat`.
    args: [--accept-license]
```

`bob run` approves every tool call by itself, so Bob needs no permission
flag. It reads its key from `BOB_API_KEY`: create a key with the
"Inference" scope at <https://bob.ibm.com> and pass it in the daemon's
environment, for example `-e BOB_API_KEY` on `podman run`, rather than in
the agent's `env`, so the key stays out of the settings file. A key
of type "general" also needs `--team-id <id>` in `args`. Bob's browser
sign-in, from `bob chat`, does not apply to `bob run`.

Sign in to each CLI once as the user that runs the daemon: on the
[Agents page](web-ui.md#signing-agents-in) of the web interface, or in a
shell with `claude auth login` or `codex login --device-auth`. The login
lives in that user's home directory.

### Setting the agent CLIs up

Two agent keys prepare a CLI before the daemon starts work, for example to
install plugins or to give the agent standing instructions:

```yaml
agents:
  claude:
    driver: claude
    command: claude
    args: [--permission-mode, bypassPermissions]
    instructions: claude-instructions.md
    setup:
      - [plugin, marketplace, add, Open-Harness-Engineering/ai-agents-oss-helper]
      - [plugin, install, oss-helper@ai-agents-oss-helper]
  codex:
    driver: codex
    command: codex
    args: [--dangerously-bypass-approvals-and-sandbox]
    instructions: codex-instructions.md
    setup:
      - [plugin, marketplace, add, Open-Harness-Engineering/ai-agents-oss-helper]
      - [plugin, add, oss-helper@ai-agents-oss-helper]
```

Each time `amaru serve`, `amaru drain` or `amaru claim --now` starts, before
it takes any work (the web page is already up):

1. The `instructions` file, when set, is copied to the file the CLI reads
   for every project: `CLAUDE.md` in `CLAUDE_CONFIG_DIR`, or in
   `~/.claude`, for Claude Code; `AGENTS.md` in `CODEX_HOME`, or in
   `~/.codex`, for Codex; `~/.bob/rules/amaru.md` for Bob, which has no
   variable for its directory. The agent's `env` is looked at first, then the
   daemon's environment. The copy replaces what is there, so edit the file
   next to the daemon file, not the copy. Removing the key leaves the last
   copy in place. The repository's own `AGENTS.md` or `CLAUDE.md` still
   applies on top of it.
2. Each `setup` entry runs as `command` with those arguments, in order, in
   the home directory, with the agent's `env`, for at most 5 minutes.

Both run at every start, because the home directory is usually a volume
that outlives the image: a new image or a new volume gets the same setup.
Every step must therefore be safe to repeat. `claude plugin marketplace
add`, `claude plugin install`, `codex plugin marketplace add` and `codex
plugin add` all are: run again, they report that the marketplace or plugin
is already there and exit 0.

A failed step is reported on stderr (`setup <agent>: failed: ...`) and the
daemon starts anyway; the next start tries again. A missing `instructions`
file or an empty `setup` entry is an error when the settings load.

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
idle_timeout: 30m

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
  reaction: thumbs_up

workspace:
  keep_blocked: 7d
```

| Key | Default | Meaning |
| --- | --- | --- |
| `repo` | required | `owner/name` of the repository whose issues the bot works on and where pull requests are opened. No trailing slash. |
| `base_branch` | `main` | The branch that ticket branches start from and target. |
| `fork` | none | `owner/name` of the bot's fork. When set, the daemon creates the fork if it is missing, branches are pushed there and pull requests come from `owner:branch`. When not set, branches are pushed to `repo` itself. See [GitHub access](github-access.md). |
| `branch_prefix` | `amaru/` | Ticket branches are `<prefix><issue>-<slug>`, for example `amaru/42-fix-the-export-button` or, for a Jira issue, `amaru/CAMEL-12345-fix-the-export-button`. Include the separator, `/` or `-`, in the prefix. |
| `agent` | required | A name from `agents:` in the daemon file. |
| `step_timeout` | `45m` | The longest one agent turn may run. |
| `idle_timeout` | `30m` | Stop an agent turn that makes no progress for this long: it prints nothing, or only retry notices such as Codex's `Reconnecting...`, for example when it cannot reach the model. The daemon then resumes the same session on its next poll, and blocks the ticket after three stalls in a row. Raise it when the agent runs builds or tests that take longer without output; `0` turns it off. |
| `checks` | `[]` | Commands the daemon runs itself after each agent turn, in order, through `sh -c` in the ticket's worktree. |
| `max_fix_rounds` | `3` | How many times failing checks go back to the agent before the ticket blocks. |
| `jira` | none | Take the tickets from a Jira project instead of the repository's GitHub issues. See [Jira](#jira) below. |

### Intake

`intake` decides which issues `amaru serve` picks up by itself. It needs
`labels`, `assignee` or both: an issue that matches either is eligible. A
project with `jira` may leave it out, and its issues are then claimed by hand
only.
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
| `reaction` | `thumbs_up` | The emoji the bot puts on each review and comment it picks up, so the reviewer sees it is being addressed: `thumbs_up`, `eyes`, `heart`, `rocket`, `hooray`, `laugh`, or `off` for none. Each one gets a single reaction, and a refused reaction does not stop the ticket. |

### Jira

The issues of a Jira project, worked on in `repo`. The [Jira guide](jira.md)
explains what changes for these tickets.

| Key | Default | Meaning |
| --- | --- | --- |
| `url` | required | The Jira server, for example `https://issues.apache.org/jira`. |
| `project` | required | The project key, for example `CAMEL`. `amaru claim CAMEL-12345` then queues that issue for this project. |
| `token_env` | `JIRA_TOKEN` | The environment variable with a personal access token. Without one, issues are read anonymously and nothing is posted. |
| `user` | none | Sign in with this user name and the token as its password, instead of sending the token as a bearer token. |
| `jql` | none | A JQL condition: matching open issues are picked up by `amaru serve`, as are those matching `intake.labels` or `intake.assignee`. |
| `comment` | `true` | Comment on the issue when work starts, stops or has a pull request. Only with a token. |

### Workspace

| Key | Default | Meaning |
| --- | --- | --- |
| `keep_blocked` | `7d` | How long the worktree of a blocked ticket is kept: a duration, `0` or `forever`. The branch is kept either way. Unblocking recreates the worktree from it. Worktrees of done and cancelled tickets are always deleted on the next poll; see `amaru clean`. |

## Prompts

The prompts are compiled into the binary. To change one, copy it from
[`prompts/`](../prompts) into a `prompts/` directory next to the daemon
file, keeping its file name, and edit it there. Prompts you don't copy keep
their built-in text.

Templates fill `{{issue}}` (`#42`, or the Jira key), `{{repo}}`, `{{number}}`,
`{{title}}`, `{{body}}`, `{{branch}}` and `{{base}}`. `implement` also gets
`{{conventions}}`, the commit message rule of a Jira project, and `continue`
gets `{{latest}}`: how to read the issue's newest comments, or for a Jira
issue the comments themselves.
