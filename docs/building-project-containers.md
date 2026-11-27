# Build a project container for Amaru Light

This guide is for coding agents preparing a project image for Amaru Light.
Produce a derived `Containerfile`, the daemon and project settings it needs,
and commands to build and verify it with Podman.

To hand this work to an agent, paste the
[coding bot prompt](coding-bot-prompt.md) into it from the project's
repository.

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
`localhost/amaru-light:latest`. The current tag uses Fedora Minimal 45 and
includes the `amaru` binary, Git, GitHub CLI, and
[ripwire](https://github.com/redhat-et/ripwire), a code map tool that helps
agents find relevant code with fewer tokens. The runtime username is still
`mini-amaru`.

Instead of a local build, you can pull the published base image,
`ghcr.io/amaru-oss/amaru-light:latest`, or a release version such as
`ghcr.io/amaru-oss/amaru-light:0.1.0`, and use it in the `FROM` line. CI
publishes it for `amd64` and `arm64` after the checks pass on `main` and on
release tags.

The image entrypoint, `/app/entrypoint`, activates the ripwire skills for
Claude Code and Codex in the runtime user's home at each start, then runs
`/app/bin/amaru`. Keep that entrypoint in derived images so the skills
reach the home volume. For Claude Code it also installs the
`ripwire-locator` subagent in `~/.claude/agents`. If the image sets
`CLAUDE_CONFIG_DIR`, the skills and the subagent go to its `skills` and
`agents` directories. Set `AMARU_RIPWIRE_SKILLS=0` to skip this step.

Build one derived image per project. Install the agent CLI and the project's
build and test tools in the derived image. Keep the inherited entrypoint and
`/app` working directory. Switch back to `USER mini-amaru` after installation.

Save the derived image definition as `Containerfile.project`. This example
assumes a Go project and a standalone Linux executable `my-agent` in the
build context. Replace the package list and agent installation step with
the versions and commands that fit your project.

```dockerfile
FROM localhost/amaru-light:latest

USER root
RUN microdnf install -y golang make && microdnf clean all
COPY --chmod=755 my-agent /usr/local/bin/my-agent

# Git over HTTPS asks gh for credentials, and gh reads GH_TOKEN. This is what
# `gh auth setup-git` writes, set here so that no step is needed at run time.
RUN git config --system credential.https://github.com.helper '' \
    && git config --system --add credential.https://github.com.helper '!/usr/bin/gh auth git-credential'

USER mini-amaru
```

Agent CLIs distributed through npm, such as Claude Code
(`@anthropic-ai/claude-code`) and Codex (`@openai/codex`), need Node.js in
the image. Codex also uses `bubblewrap` when it keeps its own sandbox.

Only copy the files the image needs. Add a `.containerignore` if the context
contains a checkout, dependency caches, local environment files, or other
artifacts. Install build tools into the image so the runtime user can invoke
them without administrative access.

## Configure the daemon

Write the daemon file, `amaru.yaml`, and a project file from the files in
[`examples/`](../examples). The [configuration reference](configuration.md)
describes every key.

- In `amaru.yaml`, define the agent CLI that the image installs. `command`
  is the executable on the container's `PATH` or an absolute path. Add the
  arguments that let it run unattended, for example
  `[--permission-mode, bypassPermissions]` for Claude Code or
  `[--dangerously-bypass-approvals-and-sandbox]` for Codex.
- In the project file, set `repo`, `fork` if the bot pushes to a fork, and
  `checks` to the project's real validation commands. They run through
  `sh -c` in the ticket's worktree.
- Set `state_dir` to a volume. The base image declares `/data` for it. To
  keep everything, state and agent logins, in one home volume, use
  `state_dir: /home/mini-amaru/amaru` instead.
- Set `web.listen: 0.0.0.0:8765` so that the [web interface](web-ui.md) is
  reachable through the published port. Inside a container, the default
  `127.0.0.1` answers only the container itself.

The image runs `amaru --config /config/amaru.yaml serve` by default. Put the
two files in `/config` one of two ways:

- Mount a host directory at `/config` (read only is fine). A settings
  change then needs only a container restart.
- Copy them into the image, so that the image is self-contained. Then
  rebuild after each change:

  ```dockerfile
  COPY --chown=mini-amaru:mini-amaru amaru.yaml project.yaml /config/
  ```

An agent's `instructions` file (its user-wide `CLAUDE.md` or `AGENTS.md`)
goes in `/config` too, next to `amaru.yaml`. The daemon copies it, and runs
the agent's `setup` steps (such as plugin installs), into the home volume
at each start; see
[Setting the agent CLIs up](configuration.md#setting-the-agent-clis-up).

The daemon clones each repository into `<state_dir>/repos` when its first
ticket starts and keeps one worktree per ticket next to the clone. For the
GitHub token, the fork and labels, see [GitHub access](github-access.md).

## Build, run, and verify

```sh
podman build -f Containerfile.project -t localhost/amaru-light-my-project:dev .

podman run -d --init --name amaru-light-my-project \
  -e GH_TOKEN \
  -p 127.0.0.1:8765:8765 \
  -v "$PWD/amaru-config:/config:ro,Z" \
  -v amaru-light-my-project-data:/data:U \
  -v amaru-light-my-project-home:/home/mini-amaru:U \
  localhost/amaru-light-my-project:dev

podman exec amaru-light-my-project /app/bin/amaru -c /config/amaru.yaml doctor
podman logs -f amaru-light-my-project
```

The volumes hold the daemon state (database, clones and worktrees) and the
runtime user's home directory. Use project-specific volume names, and use
named volumes rather than host directories for them. Leave out the `/config`
mount if the image copies the settings. `-p 127.0.0.1:8765:8765` publishes
the web interface on the host's loopback only, at <http://127.0.0.1:8765>.
To publish it on other interfaces, set `-e AMARU_WEB_TOKEN` as well; see
[Web interface](web-ui.md#access-and-the-token).

`-e GH_TOKEN` without a value copies the variable from your shell, so the
token does not show in the command line or the shell history. The volumes do
not store it; give it again when you recreate the container.

### Sign in to the agent from inside the container

Use the named volume mounted at `/home/mini-amaru` as the container's home.
Once the container is running, sign in with the installed CLI as
`mini-amaru`: on the [Agents page](web-ui.md#signing-agents-in) of the web
interface, or with the commands below. Reuse the same volume when recreating the container so file-based
login state persists. If the image sets `CODEX_HOME` or `CLAUDE_CONFIG_DIR`,
keep that directory on the volume and use the same setting for both login
and the daemon.

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
2. Check the selected agent's login status as the runtime user. If the
   image does not set the Git credential helper, run `gh auth setup-git`
   once so that Git pushes with the bot token.
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
