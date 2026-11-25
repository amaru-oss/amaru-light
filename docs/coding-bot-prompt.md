# A prompt to set up a coding bot

Each project gets its own Amaru Light image, because each project needs its
own toolchain for the bot to build and check it. The prompt below asks a
coding agent (Claude Code, Codex or another) to make that image and its
settings for you. Open the agent in your project's repository and paste it.

The agent inspects the project, asks you what it cannot find out by itself,
writes the files into a `coding-bot/` folder and builds and checks the
image. It does not start the bot on your issues; you do that when you have
read what it made.

````text
Set up an Amaru Light coding bot for this repository.

Amaru Light (https://github.com/amaru-oss/amaru-light) is a daemon that
works on GitHub issues as a bot account: it runs an agent CLI (Claude Code
or Codex) in a worktree per issue, runs the project's checks, pushes a
branch and opens a pull request, then acts on reviews and failing CI. It
never merges. It runs in a container built FROM the Amaru Light base image,
and each project has its own derived image with its own toolchain.

Read these before you write anything, from the main branch of
amaru-oss/amaru-light. They are the source of truth: when this prompt and
the docs disagree, follow the docs and tell me.
- docs/building-project-containers.md (derived images, login, verification)
- docs/configuration.md (every key; both files are checked strictly, and an
  unknown key fails at startup)
- docs/github-access.md (fork or direct push, token, labels, Git credentials)
- docs/troubleshooting.md
- docs/web-ui.md, if it exists (the web interface and its Agents page)
- examples/daemon.yaml and examples/project.yaml

## 1. Inspect this project

Read AGENTS.md, CLAUDE.md, CONTRIBUTING, the README, the build manifests and
lockfiles, the toolchain pins (rust-toolchain.toml, .nvmrc, .tool-versions,
go.mod, pom.xml, .java-version and so on) and the CI workflows. Find:
- the languages and the exact toolchain versions CI uses;
- the one command that validates a change the way CI does (for example
  `make check`, or `npm ci && npm test && npm run lint`). Prefer an existing
  Makefile target or script over a chain you invent;
- native libraries, compilers and system packages the build needs;
- anything the checks need that a container cannot give them (a browser,
  a container runtime, a database, secrets). Leave those out of the checks
  and list them for me.

## 2. Ask me what you cannot infer

Ask all of these in one message, with your best guess for each, and wait
for my answers:
1. The repository the bot works on (`owner/name`) and its base branch.
2. The bot's GitHub login. It should be a separate account, not mine.
3. Fork or direct push. Fork: the bot pushes to its own fork
   (`bot/name`, which must exist) and needs no write access upstream.
   Direct: the bot pushes to the repository and needs write access.
4. The bot's access to the repository: none, triage (organizations only)
   or write. Without it the bot cannot add its `amaru:working` and
   `amaru:blocked` labels (it goes on without them, and blocked tickets
   resume with `amaru unblock <id>`), and it cannot be assigned issues, so
   intake must use a label.
5. Which agent: Claude Code (`claude`) or Codex (`codex`), and a model, or
   the CLI's default.
6. Whose review comments the bot should act on (`review.reviewers`), and
   whether it reviews its own diff first (`review.self`: off, same_agent).
7. Which base image: `ghcr.io/amaru-oss/amaru-light:latest` (published;
   older releases are `ghcr.io/amaru-oss/mini-amaru`) or a local
   `make image` build (`localhost/amaru-light:latest`, or
   `localhost/mini-amaru:latest` before the rename). Check which name the
   docs on main use.
8. Podman or Docker (the docs use Podman), and the host port for the web
   interface if the base image has one (default 8765).

## 3. Write the files

Put everything in a `coding-bot/` folder at the repository root, and use
that folder as the build context so the checkout is not copied into the
image.

coding-bot/Containerfile
- FROM the base image I chose. Keep its ENTRYPOINT (`/app/entrypoint`),
  its CMD (`--config /config/amaru.yaml serve`), its `/app` working
  directory and the `mini-amaru` user and home (`/home/mini-amaru`).
- `USER root`, install the toolchain and system packages from step 1 with
  `microdnf` (the base is Fedora Minimal), pinned to the versions CI uses.
  Use a multi-stage `COPY --from=` when an official image is the easiest
  source (for example Node.js from `docker.io/library/node`).
- Install the agent CLI. Claude Code (`@anthropic-ai/claude-code`) and
  Codex (`@openai/codex`) come from npm, so they need Node.js. Codex also
  wants `bubblewrap`.
- Make Git get its HTTPS credentials from gh, which reads GH_TOKEN. Without
  this the clone works but the first push fails:
    RUN git config --system credential.https://github.com.helper '' \
        && git config --system --add credential.https://github.com.helper '!/usr/bin/gh auth git-credential'
- Install toolchains outside the home volume (for example under /opt) and
  owned by `mini-amaru`, so that a new image brings its own toolchain and
  the bot can still write caches and add a pinned version.
- `COPY --chown=mini-amaru:mini-amaru amaru.yaml project.yaml /config/`,
  then `USER mini-amaru`. End with a RUN that prints every tool's version,
  so a broken install fails the build.
- A header comment with the build command, the run command and the login
  step. Add a `.containerignore` if the folder can collect anything else.

coding-bot/amaru.yaml (the daemon file)
- `state_dir: /home/mini-amaru/amaru`, so that the state (database, clone,
  worktrees) and the agent logins live in one home volume. Do not use a
  path relative to /config.
- `bot_login`, `poll_interval: 60s`, `max_parallel: 1` to start with.
- If the base image has the web interface: `web: { listen: 0.0.0.0:8765 }`.
  The default 127.0.0.1 answers only inside the container.
- One agent, with the arguments for unattended work. The container is the
  sandbox, and the build usually needs the network:
    Claude Code: driver: claude, command: claude,
                 args: [--permission-mode, bypassPermissions]
    Codex:       driver: codex, command: codex,
                 args: [--dangerously-bypass-approvals-and-sandbox]
  Do not add the arguments the daemon passes itself (-p, --output-format,
  exec, --json, --resume).
- `projects: [project.yaml]`.

coding-bot/project.yaml (the project file)
- `repo`, `base_branch`, `fork` only for the fork setup, `branch_prefix`.
- `intake` needs `labels`, `assignee` or both, even if I will only use
  `claim` and `drain`. Use `labels: [amaru]`; add `assignee: <bot>` only if
  the bot has access to the repository.
- `agent`: the name under `agents:`. `step_timeout: 45m`.
- `checks`: the validation command from step 1, with a timeout that fits
  how long CI takes. They run through `sh -c` in the ticket's worktree.
  `max_fix_rounds: 3`.
- `review`: `respond: true`, `reviewers` from my answer, `fix_ci: true`.
- `workspace: { keep_blocked: 7d }`.
- Use only keys from docs/configuration.md.

coding-bot/README.md: what the folder is, in a few lines, pointing to the
longer page below.

A page for humans in the project's own docs (for example
docs/coding-bot.md, or wherever the project keeps contributor docs), with:
- Prerequisites: the bot account, its fork or write access, its role on the
  repository, and its token. Fork onto a repository someone else owns: a
  classic token with `public_repo` (`repo` if private); a fine-grained
  token cannot open the upstream pull request. Direct: a fine-grained
  token for the repository with Contents, Issues and Pull requests set to
  read and write.
- Build: the base image (pull, or `make image` in an Amaru Light checkout)
  and `podman build -f coding-bot/Containerfile -t localhost/amaru-light-<project>:dev coding-bot`.
  Say which toolchain pins must be kept in step with the Containerfile.
- Run:
    export GH_TOKEN=<bot token>
    podman run -d --name amaru-light-<project> \
      -e GH_TOKEN \
      -p 127.0.0.1:8765:8765 \
      -v amaru-light-<project>:/home/mini-amaru:U \
      localhost/amaru-light-<project>:dev
  `-e GH_TOKEN` with no value copies it from the shell, so the token is not
  in the command line or the history, and the volume never stores it.
  With Docker, drop `:U` and make sure the volume is owned by uid 1000.
  Leave out `-p` if there is no web interface. Use a different host port
  for each bot on the same machine. Bind to 127.0.0.1; to open the port
  further, set AMARU_WEB_TOKEN as docs/web-ui.md says.
- Sign the agent in once; the login stays in the volume. On the web
  interface's Agents page, or:
    podman exec -it amaru-light-<project> claude auth login
    podman exec -it amaru-light-<project> codex login --device-auth
  (Codex device login must be enabled for the ChatGPT account.)
- Optional commit identity, before the first ticket:
    podman exec amaru-light-<project> git config --global user.name <bot>
    podman exec amaru-light-<project> git config --global user.email <id>+<bot>@users.noreply.github.com
- Verify: `podman exec amaru-light-<project> /app/bin/amaru -c /config/amaru.yaml doctor`;
  every line must start with `ok`.
- Give it work: add the intake label to an issue, or
  `amaru claim 'owner/name#42'`. Quote the reference: zsh treats `#` as a
  glob and fails with "no matches found". Follow it with
  `podman logs -f`, `amaru status`, `amaru status --ticket <id>`, and the
  web interface.
- Milestones: `amaru drain owner/name --milestone "..."`. Only one daemon
  may use a state directory, so stop the serving container, run drain in a
  `podman run --rm -it` with the same volume, then start the container
  again.
- Blocked tickets: answer on the issue, then remove `amaru:blocked`, or run
  `amaru unblock <id>` when the bot cannot label.
- What the image cannot run, from step 1.

## 4. Build and verify

1. Build the image. Fix the Containerfile until it builds and the version
   check passes.
2. Run the project's checks inside the image, as `mini-amaru`, on a fresh
   clone of this repository, for example:
     podman run --rm --entrypoint sh localhost/amaru-light-<project>:dev -c \
       'git clone --depth 1 https://github.com/<owner>/<name> /tmp/p && cd /tmp/p && <check command>'
   A check that fails here will fail for every ticket. Fix the image, not
   the check, unless the check needs something a container cannot have.
3. If I gave you a token, start the container and run `amaru doctor`.
   Otherwise give me the commands to do it.
4. Do not claim an issue or start the bot on real work yourself.

## 5. Report

Tell me: the files you wrote; the image tag; the check command and whether
it passed in the image; what you verified and what you could not (for
example the agent login, which only I can do); the GitHub setup I still
have to do (account, fork, role, token, labels); and the exact commands to
start the bot and give it its first small issue.
````

## After the agent is done

Read the pull request or the files it made, then follow its report: create
the token, start the container, sign the agent in and run `amaru doctor`.
Give the bot a small issue first, and read its pull request before you give
it more.
