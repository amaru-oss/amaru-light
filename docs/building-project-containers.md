# Build a project container for Amaru Light

This guide is for coding agents preparing a project image for Amaru Light.
Produce a derived `Containerfile`, the daemon and project settings it needs,
and commands to build and verify it with Podman.

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
includes the `amaru` binary, Git, GitHub CLI, and
[ripwire](https://github.com/redhat-et/ripwire), a code map tool that helps
agents find relevant code with fewer tokens. The image name and the runtime
username are still `mini-amaru`.

Instead of a local build, you can pull the published base image,
`ghcr.io/amaru-oss/mini-amaru:latest`, or a release version such as
`ghcr.io/amaru-oss/mini-amaru:0.1.0`, and use it in the `FROM` line. CI
publishes it for `amd64` and `arm64` after the checks pass on `main` and on
release tags.

The image entrypoint, `/app/entrypoint`, activates the ripwire skills for
Claude Code and Codex in the runtime user's home at each start, then runs
`/app/bin/amaru`. Keep that entrypoint in derived images so the skills
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

USER mini-amaru
```

Only copy the files the image needs. Add a `.containerignore` if the context
contains a checkout, dependency caches, local environment files, or other
artifacts. Install build tools into the image so the runtime user can invoke
them without administrative access.

## Configure the daemon

Write `amaru.yaml` and `project.yaml` from the files in
[`examples/`](../examples) and keep them outside the image, in a directory
mounted at `/config`:

- In `amaru.yaml`, set `state_dir: /data` and define the agent CLI that the
  image installs. `command` is the executable on the container's `PATH` or
  an absolute path. Add the arguments that let it run unattended, for
  example `--permission-mode bypassPermissions` for Claude Code.
- In `project.yaml`, set `repo`, and `fork` if the bot pushes to a fork.
  Set `checks` to the project's real validation commands; they run through
  `sh -c` in the ticket's worktree.

The daemon clones each repository into `/data/repos` on first use, through
`gh`, and keeps one worktree per ticket next to it.

## Build, run, and verify

```sh
podman build -f Containerfile.project -t localhost/amaru-light-my-project:dev .

podman run -d --name amaru-light-my-project \
  -e GH_TOKEN \
  -v "$PWD/amaru-config:/config:ro,Z" \
  -v amaru-light-my-project-data:/data:U \
  -v amaru-light-my-project-home:/home/mini-amaru:U \
  localhost/amaru-light-my-project:dev

podman exec amaru-light-my-project /app/bin/amaru -c /config/amaru.yaml doctor
podman logs -f amaru-light-my-project
```

The volumes hold the daemon state (database, mirrors and worktrees) and the
runtime user's home directory. Use project-specific volume names. The
default command is `--config /config/amaru.yaml serve`.

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

1. Run `amaru doctor`. It checks the settings, Git, `gh auth status` with
   the bot token, and that each configured agent CLI starts.
2. Check the selected agent's login status as the runtime user. Run
   `gh auth setup-git` once so Git pushes with the bot token.
3. Verify that dependency caches are writable and dependency hosts resolve
   from the agent's execution environment. Run the project's check commands
   in a clone. If an agent uses its own sandbox, repeat these checks through
   that agent because a plain `podman exec` shell can have different
   permissions.
4. Claim a small test issue with `amaru claim owner/name#N --now`. Follow
   it with `amaru status --ticket <id>`, and read the pull request it opens.

Report the built image tag, the project YAML used, the commands verified,
and any remaining runtime prerequisites. State whether validation used a
real authenticated agent or a test CLI.
