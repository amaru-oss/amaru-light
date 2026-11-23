# Build a project container for Amaru Light

This guide is for coding agents preparing a project image for Amaru Light
(formerly Amaru Mini). Produce a derived `Containerfile`, the provider and project
configuration it needs, and commands to build and verify it with Podman.

## Inspect the project first

Read the target repository's `AGENTS.md`, `CLAUDE.md`, build manifests, lockfiles,
and CI workflows. Identify the build and test commands, the required toolchain
versions, and any native libraries or test services. Use those files to choose
packages and tools. For example, if CI runs `npm ci` against a committed
`package-lock.json`, preserve that package manager and lockfile workflow.

Confirm which agent CLI and models the project owner intends to use. Install
the Linux CLI and its dependencies for the image architecture. Determine how
it accepts prompts and how it authenticates in a noninteractive run. Keep
credentials and local account configuration outside the build context.

## Extend the base image

Run `make image` in the Amaru Light source checkout to build
`localhost/mini-amaru:latest`. The current tag uses Fedora Minimal 45 and
includes the Amaru Light binary, dashboard, Git, GitHub CLI, and
[ripwire](https://github.com/redhat-et/ripwire), a code map tool that helps
agents find relevant code with fewer tokens. The legacy image tag, launcher,
and runtime username still use `mini-amaru`.

The image entrypoint, `/app/entrypoint`, activates the ripwire skills for
Claude Code and Codex in the runtime user's home at each start, then runs
`/app/mini-amaru`. Keep that entrypoint in derived images so the skills
reach the home volume. If the image sets `CLAUDE_CONFIG_DIR`, the skills go
to its `skills` directory. Set `AMARU_RIPWIRE_SKILLS=0` to skip this step.

Build one derived image per project. Install the agent CLI and the project's
build and test tools in the derived image. Keep the inherited entrypoint and
`/app` working directory. Switch back to `USER mini-amaru` after installation.

Save the derived image definition as `Containerfile.project`. This example
assumes a Go project and a standalone Linux executable `my-agent` in the
build context. Replace the package list and agent installation step with
the versions and commands that fit your project.

```dockerfile
FROM localhost/mini-amaru:latest

USER root
RUN microdnf install -y golang make && microdnf clean all
COPY --chmod=755 my-agent /usr/local/bin/my-agent

COPY providers.yaml models.yaml routing.yaml /app/config/
COPY project.yaml /app/projects/project.yaml

USER mini-amaru
```

Only copy the files the image needs. Add a `.containerignore` if the context
contains a checkout, dependency caches, local environment files, or other
artifacts. Install build tools into the image so the runtime user can invoke
them without administrative access.

## Configure the agent and the checkout

- In `providers.yaml`, enable the selected CLI provider. Use the actual
  executable on the container's `PATH` or an absolute Linux path. Amaru Light
  sends the prompt over stdin and reads stdout. The `command` list is an
  argument vector, and `{model}` is replaced with the provider's model name.
- In `models.yaml`, map each logical model ID to a provider and a model listed
  in that provider's `models` list. In `routing.yaml`, assign those logical
  IDs to the routes the workflow uses, including `planning` and any
  implementation, summarization, debugging, and review routes.
- In `project.yaml`, allow those logical IDs in `models.allowed` if you
  specify an allowlist. Set `project.repository` to `owner/repository` and
  set `project.root` to a path such as `../repos/my-project`. The path is
  relative to the project YAML and resolves to `/app/repos/my-project` in
  this example. Keep the project YAML under `/app/projects` so the runtime
  finds its matching `/app/config` files.
- Set `execution.check_command` to the project's real validation command.
  The delivery loop runs it from the worktree. Amaru Light splits the
  command into arguments and launches the executable directly. If checks
  need shell operators, put them in a project script and configure the
  command to run that script.

Use the [provider examples](../config/providers.yaml),
[model definitions](../config/models.yaml), [routing defaults](../config/routing.yaml),
and [project template](../packaging/example.yaml) as references. The project
template uses manual publication and merge, which is suitable for an initial
smoke test. Supply provider credentials at runtime through the provider's
supported login flow or runtime secrets. Keep provider environment overrides
compatible with the container. For example, an old `CODEX_HOME: ~/.codex-plus`
override will take precedence over the value supplied at launch.

## Build, run, and verify

```sh
podman build -f Containerfile.project -t localhost/amaru-light-my-project:dev .

podman run -d --name amaru-light-my-project \
  -p 127.0.0.1:8765:8765 \
  -v amaru-light-my-project-data:/app/data:U \
  -v amaru-light-my-project-repos:/app/repos:U \
  -v amaru-light-my-project-home:/home/mini-amaru:U \
  localhost/amaru-light-my-project:dev \
  serve -c projects/project.yaml --host 0.0.0.0 \
  --cors-origin http://127.0.0.1:8765 --cors-origin http://localhost:8765

curl -fsS http://127.0.0.1:8765/api/health
podman exec amaru-light-my-project /app/mini-amaru doctor -c projects/project.yaml
```

The volumes hold the job database, logs and worktrees, project checkouts,
and the runtime user's home directory, respectively. Use project-specific
volume names. The dashboard uses the host port above. If you change that
port, update each browser origin accordingly.

### Sign in to the agent from inside the container

Use the named volume mounted at `/home/mini-amaru` as the container's home.
Once the container is running, the user signs in with the installed CLI as
`mini-amaru`. Reuse the same volume when recreating the container so file-based
login state persists. If the image sets `CODEX_HOME` or `CLAUDE_CONFIG_DIR`,
keep that directory on the volume and use the same setting for both login
and jobs.

For Codex with a ChatGPT account, use device login and open the displayed
URL in a host browser. Device login must be enabled for the account or
workspace. See the [Codex headless login guide](https://learn.chatgpt.com/docs/auth#login-on-headless-devices).

```sh
podman exec -it amaru-light-my-project codex login --device-auth
podman exec amaru-light-my-project codex login status
```

For Claude Code, run its own login flow and complete the browser steps it
prints in the terminal. See the [Claude Code authentication guide](https://code.claude.com/docs/en/authentication).

```sh
podman exec -it amaru-light-my-project claude auth login
podman exec amaru-light-my-project claude auth status
```

The host settings directory is optional. If you want to reuse preferences,
copy or mount individual settings files after checking for host-specific
paths, hooks, and credential helpers. A macOS Keychain login stays on the
host even if you mount the CLI's settings directory into the Linux container.
For unattended API-key authentication, use the selected provider's runtime
secret mechanism. Keep credentials out of the image and committed YAML.

### Verify the runtime

1. Confirm that health reports the expected project and `doctor` resolves
   the planning model. A missing checkout is expected on a fresh volume.
   When a job first needs it, Amaru Light can clone the configured repository
   through `gh`.
2. Check the selected agent's login status and authenticate GitHub CLI
   separately inside the container as the runtime user. For GitHub over
   HTTPS, run `gh auth setup-git` after login. Set a Git name and email in the
   container if the workflow creates commits.
3. Verify the checkout and dependency caches are writable and dependency
   hosts resolve from the agent's execution environment. Run the project's
   check command there. If an agent uses its own sandbox, repeat these checks
   through that agent because a plain `podman exec` shell can have different
   permissions.
4. Run a small job in a test checkout. Inspect its agent output and test
   results. The job status alone may reflect a successful agent process
   even when its report says the task was blocked.

Report the built image tag, the project YAML used, the commands verified,
and any remaining runtime prerequisites. State whether validation used a
real authenticated agent or a test CLI.
