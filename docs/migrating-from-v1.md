# Migrating from v1

Amaru Light v2 replaces the v1 job runtime and delivery loop with a daemon
that turns issues into pull requests. The settings, the commands and the
container layout all changed. A v1 configuration does not load: v2 rejects
keys it does not know, so the daemon stops at startup and names the
first unknown key.

This page lists what moved, what is gone and how to convert a v1 project.

## What you no longer need

| v1 file | v2 |
| --- | --- |
| `config/providers.yaml` | The `agents:` section of the daemon file. |
| `config/models.yaml` | `model:` on each agent. There are no logical model IDs. |
| `config/routing.yaml` | Gone. Each project names one agent; self-review can name a second one. |
| `config/runtime.yaml` | `state_dir`, `poll_interval` and `max_parallel` in the daemon file. |
| `projects/<name>.yaml` | A project file in the new format; see below. |
| `skills/` | Gone. The prompts are built in and can be overridden from a `prompts/` directory. Skills installed for the agent CLI itself, such as the ripwire skills in the image, still work. |
| `data/`, `repos/` | Replaced by `state_dir`. The daemon clones the repository there itself. |

You can delete the v1 files once the new ones load. The v1 database is not
read; tickets start fresh.

## Commands

| v1 | v2 |
| --- | --- |
| `mini-amaru serve` (API and dashboard) | `amaru serve`: the daemon. It has no HTTP API or web page. |
| `mini-amaru run -p "..."` | `amaru claim owner/name#42 [--now]`: work comes from issues, not prompts. |
| `mini-amaru loop -p "Milestone 5"` | `amaru drain owner/name --milestone "Milestone 5"` |
| `mini-amaru jobs`, `status --job` | `amaru status`, `amaru status --ticket <id>` |
| `mini-amaru resume --job` | `amaru unblock <id>` |
| `mini-amaru doctor` | `amaru doctor` |

Every command takes `-c <daemon file>`. Ticket IDs are the daemon's own
numbers, shown by `amaru status`; they are not issue numbers.

Follow the daemon with its log (stderr, or `podman logs -f` in a
container), with `amaru status`, and on the issues: the bot comments on an
issue when it starts and when it needs you.

## Converting a project file

| v1 key | v2 |
| --- | --- |
| `project.repository` | `repo`. Drop any trailing slash. |
| `project.name` | Gone; the repository name is used. |
| `project.root` | Gone. The clone lives under `state_dir`. |
| `git.base_branch` | `base_branch` |
| `git.branch_prefix` | `branch_prefix`. Branches are now `<prefix><issue>-<slug>`, so `feat/` gives `feat/42-<slug>`. |
| `git.branch_prefixes` (by label) | Not supported. Every branch uses `branch_prefix`. |
| `git.fork_repository` | `fork` |
| `execution.check_command`, `check_timeout_seconds` | A `checks:` entry with `run` and `timeout`. |
| `execution.require_tests` | Gone. Put your test command in `checks`. |
| `execution.max_parallel_agents` | `max_parallel` in the daemon file (1 or 2). |
| `loop.skip_labels` | `intake.skip_labels`. Keep `amaru:hold` in the list if you use it. |
| `loop.max_iterations`, `max_tasks_per_item`, `on_blocked`, `on_waiting`, `brain_route`, `worktrees_dir` | Gone. One agent session works the whole issue. A ticket that needs you blocks on its own, and the others go on. |
| `loop.create_integration_branch` | Not supported. A dependent issue stacks on the branch of the issue it depends on instead. |
| `publish.mode` | Gone. The daemon always opens the pull request. |
| `review.enabled`, `review.max_rounds` | `review.self` (`same_agent` or `other_agent` for an agent review before the PR, `off` to rely only on human reviews) and `review.max_rounds`. Also set `review.reviewers`: only their feedback reaches the agent. |
| `merge.*` | Gone. The daemon never merges. |
| `models.allowed` | Gone. |
| `skills.*` | Gone; see above. |
| `github.cli` | `gh` in the daemon file. |
| `github.milestone`, `exit_issue_*`, `publish_report` | Gone. Pass the milestone to `amaru drain`. |

Then decide how the daemon finds work on its own with `intake.labels`
and/or `intake.assignee`. A project needs at least one of them, even if you
only plan to use `claim` and `drain`.

### Example

A v1 project that pushed to a fork:

```yaml
project:
  name: my-app
  repository: owner/my-app
  root: ../repos/my-app
git:
  base_branch: main
  branch_prefix: feat/
  fork_repository: my-bot/my-app
execution:
  check_command: make check
review:
  enabled: true
  max_rounds: 1
merge:
  mode: manual
```

becomes the project file:

```yaml
repo: owner/my-app
base_branch: main
fork: my-bot/my-app
branch_prefix: feat/

intake:
  labels: [amaru]
  assignee: my-bot

agent: codex

checks:
  - name: check
    run: make check
    timeout: 30m

review:
  self: same_agent
  reviewers: [owner]
  max_rounds: 1
```

and a daemon file next to it:

```yaml
state_dir: data
max_parallel: 1
bot_login: my-bot

agents:
  codex:
    driver: codex
    command: codex
    args: [--dangerously-bypass-approvals-and-sandbox]

projects:
  - project.yaml
```

## Container images

| v1 | v2 |
| --- | --- |
| `/app/mini-amaru` launcher | `/app/bin/amaru`. The entrypoint `/app/entrypoint` still activates the ripwire skills first. |
| Default command `serve --host 0.0.0.0 ...`, port 8765 | `--config /config/amaru.yaml serve`. No port to publish. |
| `/app/config/*.yaml`, `/app/projects/*.yaml` | The daemon file and project files in `/config/`, mounted or copied into the image. |
| `/app/data`, `/app/repos` | `state_dir`. The base image declares a `/data` volume for it; you can also put `state_dir` in the home volume. |
| `curl /api/health` | `amaru -c /config/amaru.yaml doctor` |

The image is now published as `ghcr.io/amaru-oss/amaru-light` (it was
`ghcr.io/amaru-oss/mini-amaru`); update the `FROM` line of derived images. The
runtime user (`mini-amaru`, home `/home/mini-amaru`) did not change, so
existing home volumes with agent logins keep working.

See [Build a project container](building-project-containers.md) for a
complete derived image.
