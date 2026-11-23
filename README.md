# Mini-Amaru

Experimental local runtime in **Rust** for coordinating engineering agents through their CLIs. SQLite + YAML + Git/GitHub CLI. The React/TypeScript frontend lives in `web/`. No Python installation is required.

## Requirements and installation

- Rust and Cargo (stable toolchain with support for the 2024 edition)
- Git; GitHub CLI (`gh`) authenticated for milestones, issues and PRs
- At least one agent CLI installed and authenticated
- Node.js/npm, only for the frontend

```bash
make install
cp packaging/example.yaml projects/my-project.yaml
```

The binary is placed at `target/release/mini-amaru`. Use it directly or run the `make`/`cargo run` shortcuts below.

## Package for another machine

```bash
make package
# Optional: another target, with the toolchain/linker already installed:
make package TARGET=x86_64-unknown-linux-musl
```

This produces `dist/mini-amaru-<version>-<target>.tar.gz` and a SHA-256 checksum. The package includes
the release binary, the compiled frontend, YAML configuration, projects, skills and
the installer. It does not include databases, logs, credentials, checkouts or caches. The build
needs Rust/Cargo and Node.js/npm; the target machine does not need them.
Each package targets one operating system/architecture; cross-compilation requires
the target's tools. A macOS package does not run on Linux. Linux GNU builds also
require a glibc compatible with the build machine.

Validate a native package with `scripts/test-package.sh dist/<file>.tar.gz`.

Copy the file to the other machine, extract it and run:

```bash
tar -xzf mini-amaru-<version>-<target>.tar.gz
cd mini-amaru-<version>-<target>
./install.sh "$HOME/bot/mini-amaru"
cd "$HOME/bot/mini-amaru"
# Configure projects/example.yaml and config/providers.yaml.
./mini-amaru doctor
./mini-amaru serve
```

Open **http://127.0.0.1:8765**: the package launcher serves the UI and the API on
the same port. Git, `gh` and the chosen agent CLI must be installed and
authenticated on the target. The installer uses a user-writable directory and
refuses to overwrite existing installations. Full instructions, SSH access
and SQLite permission troubleshooting are in `INSTALL.md` inside the package.

Edit `projects/my-project.yaml`: `project.root` points to the project checkout (relative to the YAML file or absolute); `project.repository` is `owner/repo`; `models.allowed` restricts models; `execution.check_command` defines validation. The files in `config/` select providers, models, routes, timeouts and storage. Provider commands receive the prompt on stdin and substitute `{model}`.

The database, artifact and worktree directories are created automatically.
When a job or loop starts, if `project.root` is missing or empty and
`project.repository` is set, the runtime clones that repository with `gh repo clone`.
Authenticate `gh` as the service user. Existing checkouts with files
are preserved. Without `project.repository`, a missing directory is created as
an empty workspace. `doctor` and `loop --dry-run` do not clone repositories.

```bash
make doctor CONFIG=projects/my-project.yaml
make run CONFIG=projects/my-project.yaml PROMPT='Implement feature X with tests.'
make jobs CONFIG=projects/my-project.yaml
make status CONFIG=projects/my-project.yaml JOB=1
```

CLI equivalent:

```bash
cargo run -- -c projects/my-project.yaml run -p 'Implement feature X with tests.'
cargo run -- -c projects/my-project.yaml resume --job 1
cargo run -- --help
```

`-c/--config` can appear before or after the subcommand. `status` and `jobs` use the database of the selected configuration.

## Two execution modes

**Job (`run`)**: the planning model generates JSON tasks. Each task runs in a new provider call, sequentially, with instructions and recent summaries. The runtime records attempts, events, output and a report. Agents work directly in `project.root` and are instructed not to commit/push/merge. `resume` creates a new job with the original request and continuation instructions.

**Delivery loop (`loop`)**: a persistent state machine watches GitHub, picks eligible work and coordinates its delivery:

```text
SYNC -> OBSERVE -> DECIDE -> WORK -> SYNC ...
                         |
                         PREPARE -> EXECUTE -> VERIFY -> PUBLISH -> REVIEW -> MERGE
```

The software validates the brain's decisions and uses a deterministic fallback if the model returns an invalid action or is unavailable. Each item becomes a child job in a separate Git worktree. Agents implement and commit; the runtime creates branches, runs verification, publishes PRs, requests review and applies the merge policy.

The loop respects blockers and blockers inherited from the parent, sub-issues, open PRs and human-gate labels (`owner-task`, `awaiting-owner-approval`, `milestone-exit`). Sub-issues use the parent's integration branch. Partial PRs reference the issue; closing it requires declared completion and the integration rules. A PR from the parent to the main branch remains a human decision.

```bash
make loop-dry GOAL='Continue the Milestone 5 tasks until it is completed'
make loop GOAL='Continue the Milestone 5 tasks until it is completed'
cargo run -- loop -c projects/amaru.yaml --milestone 5 -p 'Continue the milestone'
cargo run -- loop -c projects/amaru.yaml --resume 12
```

`--dry-run` queries the state without agents, worktrees, push or PR. `Milestone N` first tries the title `M<N> ...`, before GitHub's internal number. Check the selection in the dry run.

Main policies:

- `publish.mode`: `auto` or `manual` (pauses before publishing).
- `merge.mode`: `manual`, `integration_only` (default; only integration is automatic) or `always`.
- `review.enabled`, `review.max_rounds`: independent review and fix-round limit.
- `loop.max_iterations`, `loop.max_tasks_per_item`, `loop.max_passes_per_item`: work limits.
- `loop.on_blocked`: `stop` or `brain`; `loop.on_waiting`: `stop` or `continue`.

Review and checks gate automatic merges. Manual approval also requires that checks are not pending or failing. The loop can be resumed under the same ID from its saved state. Cancellation is cooperative and checked between steps; it does not immediately interrupt an agent call in progress.

### Contributing through a fork

Create the fork on GitHub and configure the project YAML:

```yaml
project:
  name: my-project
  repository: organization/my-project
  root: ../repos/my-project
git:
  base_branch: main
  fork_repository: my-user/my-project
```

Keep `origin` pointing to the upstream repository. If the checkout does not
exist yet, the runtime clones `project.repository` on first use. The `gh`
account must be able to access upstream, push branches to the fork and open
PRs on upstream. Run `gh auth setup-git` on the machine or in the container
that runs the loop to authenticate HTTPS pushes.

In the delivery loop, branches start from upstream, pushes go to the fork
and the PR is opened on upstream. `publish.mode: manual` still lets you review the
work before publishing. A maintainer merges on GitHub and the loop
reconciles the result on the next sync. If a sub-issue requires
an integration branch that does not exist yet, ask a maintainer to
create it on upstream. Without `git.fork_repository`, the flow uses `origin`
for publishing as well.

## Local API and frontend

```bash
make serve CONFIG=projects/my-project.yaml
# Or:
cargo run -- -c projects/my-project.yaml serve --port 9000 --cors-origin http://localhost:5173
```

The server listens on `127.0.0.1:8765` by default, with a single sequential worker. Browser origins are validated on the server; the default origins are `http://localhost:5173` and `http://127.0.0.1:5173`.

| Method | Endpoint | Purpose |
| --- | --- | --- |
| GET | `/api/health` | Service health |
| GET | `/api/jobs` | Recent jobs |
| POST | `/api/jobs` | `{ "prompt": "...", "mode": "job" or "loop", "milestone": "5" }` |
| GET | `/api/jobs/<id>` | Job, tasks, child items and available actions |
| GET | `/api/jobs/<id>/events?after=0` | Incremental events |
| GET | `/api/jobs/<id>/log?offset=0` | Incremental output `{text, offset, size}` |
| POST | `/api/jobs/<id>/resume` | Continues a loop or creates a continuation job |
| POST | `/api/jobs/<id>/cancel` | Requests loop cancellation |
| POST | `/api/jobs/<id>/pr` | Publishes the PR of a ready item |
| POST | `/api/jobs/<id>/approve` | Approves and merges the PR |

Creation requires `Content-Type: application/json`; the body is limited to 64 KiB and the prompt to 20,000 characters. The existing frontend consumes the same API:

```bash
cd web
npm install
npm run dev
```

The UI offers a dashboard, a list of runs, a goal form, details, tasks, logs/events, PR publishing, approval and resuming. `VITE_API_URL` overrides the default API URL. See `web/README.md`.

To serve the compiled frontend from the binary itself: `VITE_API_URL='' npm --prefix web run build`
and `cargo run -- -c projects/my-project.yaml serve --web-dir web/dist`.

## Podman

On macOS, start the VM with `podman machine start` (run `podman machine init`
first if you do not have a VM yet). From the project root, run `make image`.
The base image `localhost/mini-amaru:latest` uses Fedora Minimal 45 and includes the
Linux binary, the dashboard, Git, `gh` and [ripwire](https://github.com/redhat-et/ripwire).
Build a derived image for each project, with the agent CLI and the tools
required by the build and tests.

ripwire is a code map tool that lets agents find the code for a task in few
tokens. The `RIPWIRE_VERSION` build argument selects the release tag to build,
for example `podman build --build-arg RIPWIRE_VERSION=v0.6.10 ...`.
On x86-64 it needs a CPU with x86-64-v3 instructions (AVX2). At each start,
the container entrypoint activates the ripwire skills for Claude Code
(`~/.claude/skills`) and Codex (`~/.agents/skills`) in the runtime user's
home, so they also reach a home directory mounted as a volume. Set
`AMARU_RIPWIRE_SKILLS=0` to skip that step.

For example, if your project uses Go and you already have a Linux-compatible
agent CLI, save a `Containerfile.project` in your build context:

```dockerfile
FROM localhost/mini-amaru:latest
USER root
RUN microdnf install -y golang make && microdnf clean all
COPY --chmod=755 my-agent /usr/local/bin/my-agent
COPY providers.yaml /app/config/providers.yaml
COPY models.yaml /app/config/models.yaml
COPY routing.yaml /app/config/routing.yaml
COPY project.yaml /app/projects/project.yaml
USER mini-amaru
```

Use the YAML files in `config/` and `packaging/example.yaml` as a reference.
Configure the agent command in `providers.yaml` to receive the prompt on
stdin and write the response to stdout. The IDs in `routing.yaml` must
exist in `models.yaml` and be allowed by the project. Set
`project.root: ../repos/my-project` and the project's check command in
`project.yaml`. Keep credentials out of the build context.

```sh
podman build -f Containerfile.project -t localhost/mini-amaru-project:latest .
```

Use named volumes to persist jobs, worktrees, checkouts and the container
account's configuration.

```sh
podman run -d --name mini-amaru \
  -p 127.0.0.1:8765:8765 \
  -v mini-amaru-data:/app/data:U \
  -v mini-amaru-repos:/app/repos:U \
  -v mini-amaru-home:/home/mini-amaru:U \
  localhost/mini-amaru-project:latest \
  serve -c projects/project.yaml --host 0.0.0.0 \
  --cors-origin http://127.0.0.1:8765 --cors-origin http://localhost:8765
```

Open **http://127.0.0.1:8765** on the machine where Podman publishes the port.
Before the first job, authenticate the agent installed in the derived image and
`gh` inside the container. The configuration in the home directory persists in
the `mini-amaru-home` volume.

```sh
podman exec -it mini-amaru gh auth login --hostname github.com --git-protocol https --web
podman exec mini-amaru gh auth setup-git
podman exec mini-amaru /app/mini-amaru doctor -c projects/project.yaml
```

Set `git config --global user.name` and `user.email` inside the container
if the agents need to create commits. For another project, mount the configuration
with `-v "$PWD/projects:/app/projects:ro,Z"` and select the YAML with `-c`. Use a
relative path such as `../repos/my-project` in the YAML. The first job can clone
the repository into the volume using `gh` authentication. On SELinux hosts, use
`:Z` to label the bind mounts. If you change the published port, adjust the origins
to the URL the browser uses.

See [docs/building-project-containers.md](docs/building-project-containers.md) for a full guide.

## Data and configuration

- `config/runtime.yaml`: SQLite, artifacts and retries.
- `config/providers.yaml`: CLIs and the Ollama endpoint (`/api/generate`).
- `config/models.yaml`: logical model IDs.
- `config/routing.yaml`: model per task type and availability fallback.
- `projects/*.yaml`: checkout, policies and limits per project.
- `skills/`: instructions added to prompts.
- `data/mini-amaru.db`: jobs, tasks, attempts, events, artifacts and loop state.
- `data/artifacts/`: plans, GitHub snapshots, reports and logs.

The SQLite schema and existing YAML configuration are preserved. Databases from the previous version are migrated by adding missing metadata; make a backup before the first run of the new version. Jobs that were RUNNING/QUEUED when the server starts become INTERRUPTED.

## Operational limits

This runtime is experimental. Worktrees separate changes, but they are not a sandbox. Providers and checks run with the user's permissions and may inherit credentials from the environment. Restrictions in prompts depend on the CLI's behavior; use an isolated environment and limited permissions for untrusted content.

The API has no authentication; keep the bind local and do not expose the port. The loop can publish and integrate real changes according to the policy. Start with a dry run, `publish.mode: manual`, `merge.mode: manual` and low limits. Job mode trusts the agent process's success; a report does not prove the code is correct.

## Development

```bash
make check
# cargo fmt --all -- --check
# cargo clippy --locked --all-targets -- -D warnings
# cargo test --locked
```

The tests use Rust, fake shell agents and temporary Git repositories. They need no Python, credentials or real agents. API/CLI tests run the binary; delivery tests cover worktrees, publishing, review, approval and resuming with a fake GitHub CLI.

Rust layout: `src/main.rs` (CLI), `server.rs` (HTTP), `engine.rs`/`providers.rs`/`runtime.rs` (execution), `delivery.rs` (loop and items), `github.rs`/`gitops.rs`/`sources.rs`/`policy.rs` (integration and rules), `storage.rs`/`config.rs`/`joblog.rs` (infrastructure).

License: experimental use.
