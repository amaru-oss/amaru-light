# Amaru Light

A personal daemon that turns GitHub issues, or Jira issues, into pull requests. It claims
issues, works each one in a single agent CLI session (Claude Code, Codex or IBM Bob),
runs the project's checks, opens a pull request and keeps fixing it from
review feedback and CI until you merge it. It never merges.

It is built for one person on a laptop, a home server or a cloud VM, not for a
team. The design and the reasons behind it are in [docs/design.md](docs/design.md).

| Guide | For |
| --- | --- |
| [Web interface](docs/web-ui.md) | Following tickets, claiming issues and signing agent CLIs in from the browser. |
| [Jira tickets](docs/jira.md) | Working on Jira issues, such as the ASF's `CAMEL-12345`, with the code and pull requests on GitHub. |
| [Configuration reference](docs/configuration.md) | Every key of the daemon and project files, with defaults. |
| [GitHub access](docs/github-access.md) | The bot account, its token, fork or direct pushes, and what works without write access. |
| [Build a project container](docs/building-project-containers.md) | A derived image with your agent CLI and toolchain. |
| [The coding bot](docs/coding-bot.md) | Amaru Light's own bot: building and running the image in `coding-bot/`. |
| [A prompt to set up a coding bot](docs/coding-bot-prompt.md) | Paste it into a coding agent in your project to get its image, settings and docs. |
| [Troubleshooting](docs/troubleshooting.md) | Following a ticket, and the errors you are likely to meet first. |
| [Migrating from v1](docs/migrating-from-v1.md) | Converting v1 settings, commands and containers. |

## How it works

```
claim → QUEUED → PREPARING → IMPLEMENTING → CHECKING → SELF_REVIEW → PUBLISHING
      → AWAITING_REVIEW ⇄ ADDRESSING_REVIEW → DONE (after you merge)
any step → BLOCKED (with the reason on the issue) → resumes when unblocked
```

- **One agent session per ticket.** Implementation, check fixes, self-review
  and review rounds all resume the same CLI session, so the agent keeps its
  context and the prompt cache. If a session is lost, the daemon starts a new
  one with a compact context pack.
- **A result file, not prose.** At the end of every step the agent writes
  `.amaru/result.json` (done, blocked or needs clarification, with a summary).
  The daemon reads it instead of parsing the agent's output.
- **Deterministic checks.** The daemon runs the project's checks itself and
  sends the failures back to the same session, up to `max_fix_rounds`.
- **At most two tickets in progress.** A ticket waiting for review frees its
  slot. An issue that depends on another one (`Depends on #12` in its body)
  starts once #12 has a pull request, on a branch stacked on it.
- **Restartable.** Tickets live in SQLite. After a restart or a crash every
  unfinished ticket continues from its last state.

## Ways to claim work

| Mode | Command | What is claimed |
| --- | --- | --- |
| Autonomous | `amaru serve` | Issues with one of the project's `intake.labels` and/or assigned to `intake.assignee` |
| Milestone | `amaru drain owner/name --milestone "v1.2"` | Every open issue of the milestone, then the command exits |
| Manual | `amaru claim owner/name#42 [--now]`, or `amaru claim CAMEL-12345` for a [Jira project](docs/jira.md) | One issue you pick |

Issues with a skip label (`amaru:hold`, `needs-human` by default) are left
alone. Priority labels, then the milestone due date, then age decide the
order.

While the daemon works on an issue it adds the `amaru:working` label. When it
needs you, it comments on the issue with the reason and adds `amaru:blocked`.
Remove the label, or run `amaru unblock <ticket>`, to let it continue where it
stopped. Closing the issue cancels the ticket.

Labelling needs write access to the repository. A bot that contributes
through a fork often has none: then the daemon works without the labels,
logs that it skipped them, and its comment on a blocked issue says to run
`amaru unblock <ticket>` instead.

When the agent is stuck, or going the wrong way, tell it what to do:
`amaru guide <ticket> "the pr-build failure is a flaky test; mark it and move on"`,
or the **Guide the agent** box on the ticket's web page. The directions go
into the agent's own session, so it keeps everything it knew. A blocked
ticket continues with them; a running agent is stopped and resumed with
them; a ticket waiting for review runs a turn for them, the checks, and
pushes again. The daemon acts on them at its next poll.

## Reviews

After the pull request is open, the daemon polls it. Review comments,
reviews that request changes and failing CI start a new round in the same
session, up to `review.max_rounds`. Only feedback from the logins in
`review.reviewers` reaches the agent, and the bot's own comments are ignored.
The bot puts a 👍 on each review and comment it picks up (`review.reaction`),
so the reviewer can see it is being addressed.

Self-review is optional per project: `off` (only act on reviews received),
`same_agent` (the working session reviews its own diff) or `other_agent` (a
fresh session of `review.reviewer_agent` reviews it).

## Disk space

Each ticket gets its own worktree, and builds in it (`target/`,
`node_modules/`) can take gigabytes. The daemon deletes a worktree, with
its build output, as soon as nothing will run in it again: on every poll it
removes the worktrees of tickets that are done or cancelled, and any
worktree no unfinished ticket uses, including those left by older versions.
A blocked ticket keeps its worktree for `workspace.keep_blocked` (7 days by
default). Branches always stay, so unblocking recreates the worktree.

`amaru clean` does the same at once, also while the daemon runs;
`--blocked` also deletes the worktrees of blocked tickets, and `--dry-run`
lists what would go and how much space it takes.

## Direct pushes or a fork

Without `fork`, branches are pushed to the repository itself. With
`fork: my-bot/my-project`, they are pushed to the bot's fork and pull
requests are opened upstream from `my-bot:branch`. A stacked pull request
from a fork targets the base branch, because GitHub cannot open a pull
request against a branch of the fork; its body names the pull request it
builds on.

The daemon, not the agent, sets this up before every agent turn and push:
it creates the fork when it is missing, and keeps `origin` = the fork and
`upstream` = the repository in the clone and every worktree. A fork it
cannot use stops the ticket before the agent runs. See
[GitHub access](docs/github-access.md).

A bot that pushes to a fork usually cannot label the upstream issues. The
daemon then works without the labels, and you unblock a ticket with
`amaru unblock <ticket>`. See [GitHub access](docs/github-access.md).

## Requirements

- Git and the GitHub CLI (`gh`).
- A GitHub token for the bot account in `GH_TOKEN`. A separate bot account
  keeps its comments and pull requests apart from yours. Run
  `gh auth setup-git` once so Git pushes with that token. Which token to
  create depends on direct or fork pushes; see
  [GitHub access](docs/github-access.md#the-token).
- At least one agent CLI, signed in: `claude`, `codex` or `bob` (IBM Bob, with
  `BOB_API_KEY`). The agent runs
  unattended in its worktree, so it must be allowed to run the project's
  build and tests without asking.
- Rust (see `rust-toolchain.toml`) to build from source.

## Quick start

```sh
make install                     # target/release/amaru
mkdir -p ~/amaru && cp examples/*.yaml ~/amaru/
mv ~/amaru/daemon.yaml ~/amaru/amaru.yaml
# Edit ~/amaru/amaru.yaml: the agent (Claude Code, Codex or IBM Bob).
# Edit ~/amaru/project.yaml: repo, fork, intake, agent, checks, review.
export GH_TOKEN=...              # the bot account's token
target/release/amaru -c ~/amaru/amaru.yaml doctor
target/release/amaru -c ~/amaru/amaru.yaml serve
```

Other commands:

```sh
amaru -c amaru.yaml claim 'owner/name#42'          # queue one issue
amaru -c amaru.yaml drain owner/name --milestone "v1.2"
amaru -c amaru.yaml status              # recent tickets
amaru -c amaru.yaml status --ticket 7   # one ticket's events
amaru -c amaru.yaml unblock 7
amaru -c amaru.yaml guide 7 "skip the docs build; it needs network"
amaru -c amaru.yaml guide 7 --file directions.md
amaru -c amaru.yaml cancel 7
```

Ticket IDs (`7` above) are the daemon's own numbers from `amaru status`, not
issue numbers. Quote issue references in zsh, where `#` can be a glob
character. Run one daemon per state directory: stop `serve` before a
`drain`.

## Configuration

Two kinds of YAML files, both checked strictly (an unknown key is an error,
so a v1 configuration does not load; see
[Migrating from v1](docs/migrating-from-v1.md)):

- [`examples/daemon.yaml`](examples/daemon.yaml): machine-wide settings. The
  state directory, poll interval, parallel tickets (1 or 2), the agent CLIs
  and the list of project files.
- [`examples/project.yaml`](examples/project.yaml): one repository. Base
  branch, optional fork, intake selectors, the agent to use, step timeout,
  checks, fix rounds, review and self-review, and how long the worktree of a
  blocked ticket is kept (`workspace.keep_blocked`: `0`, `7d`, `forever`).

The state directory holds `amaru.db` (tickets and their event log) and
`repos/<owner>/<name>/`, a mirror clone with one worktree per ticket.

Every key is described in the [configuration reference](docs/configuration.md).

The prompts are in [`prompts/`](prompts) and compiled into the binary. To
change one, put a file with the same name in a `prompts/` directory next to
the daemon settings file.

## Package for another machine

```sh
make package
# Optional: another target, with the toolchain and linker installed:
make package TARGET=x86_64-unknown-linux-musl
scripts/test-package.sh dist/amaru-<version>-<target>.tar.gz
```

The archive holds the binary, the example settings, an installer and
[INSTALL.md](packaging/INSTALL.md). It has no database, credentials or
checkouts.

## Container image

```sh
make image          # localhost/amaru-light:latest
make image-check    # build it and run scripts/test-image.sh
```

The base image (Fedora Minimal) contains `amaru`, Git, the GitHub CLI and
[ripwire](https://github.com/redhat-et/ripwire), a code map tool that helps
agents find the code for a task in few tokens. The entrypoint activates the
ripwire skills for Claude Code and Codex at each start
(`AMARU_RIPWIRE_SKILLS=0` skips it). By default the image runs
`amaru --config /config/amaru.yaml serve`; mount your settings at `/config`
and a volume at `/data`, with `state_dir: /data`.

Derive one image per project with its agent CLI and build tools:
[docs/building-project-containers.md](docs/building-project-containers.md).

CI publishes the image to `ghcr.io/amaru-oss/amaru-light` for `amd64` and
`arm64`: `latest` and the commit SHA after a push to `main`, the version and
the commit SHA after a release tag.

## Safety

Worktrees separate changes, but they are not a sandbox. The agent and the
checks run with the daemon user's permissions and environment. Run the daemon
in a container or a VM, with a bot token that can only reach the repositories
it works on. The daemon pushes only to the branches it created and never
merges.

## Development

```sh
make check          # cargo fmt --check, clippy -D warnings, cargo test
make image-check    # build the base image and smoke-test it (podman or docker)
```

The crate follows the lint configuration of
[Wanaku](https://github.com/wanaku-ai/wanaku) (`clippy.toml` and `[lints]` in
`Cargo.toml`): no `unwrap`, `expect`, `panic` or `unsafe` outside tests, and
short functions. The tests use fake agent and `gh` scripts and temporary Git
repositories; they need no credentials.

`rust-toolchain.toml` pins the Rust release. Keep it on the same release as
the Rust stage of the `Containerfile`.

| Module | Role |
| --- | --- |
| `bin/amaru.rs`, `app.rs` | Command line and wiring |
| `scheduler` | Intake, slots, polling pull requests, retention |
| `lifecycle` | One ticket's steps, sessions and review rounds |
| `agent` | Claude Code, Codex and IBM Bob drivers |
| `contract.rs`, `prompts.rs` | The result file and the prompts |
| `forge` | GitHub issues and pull requests through `gh`, Jira issues through `curl` |
| `worktree.rs`, `checks.rs`, `process.rs` | Git, checks and child processes |
| `db.rs`, `ticket.rs`, `settings.rs` | State and configuration |

### Continuous integration

`.github/workflows/ci.yml` runs on each pull request, each push to `main`
and each release tag: `make fmt-check`, `make clippy`, `make test`, and an
image build with `scripts/test-image.sh` on `amd64` and `arm64`. After all
checks pass, pushes to `main` and release tags publish the image. The
optional repository variables `CONTAINER_REGISTRY` (default `ghcr.io`) and
`CONTAINER_GROUP` (default the repository owner) change its location.

### Releases

1. Set `version` in `Cargo.toml`, run `cargo check` so `Cargo.lock` agrees,
   and merge the change.
2. Push a bare version tag on that commit: `git tag 0.1.0 && git push origin 0.1.0`.

CI stops when the tag differs from the version in `Cargo.toml`.

## License

Amaru Light is licensed under the [Apache License, Version 2.0](LICENSE).
