# The coding bot

Amaru Light works on its own issues. The bot account `orp-bot` runs the
daemon in a container built from [`coding-bot/`](../coding-bot): it picks
issues labelled `needs-implementation`, implements them with Codex, runs
`make check`, pushes to its fork `orp-bot/amaru-light` and opens a pull
request here. It reviews its own diff first, then acts on reviews from
`orpiske` and on failing CI. It never merges.

## Prerequisites

- The bot account `orp-bot`. The daemon creates its fork of this
  repository, `orp-bot/amaru-light`, on the first ticket if it is missing.
- Its role on `amaru-oss/amaru-light`. With no role the bot still works
  through the fork, but it cannot add the `amaru:working` and
  `amaru:blocked` labels and cannot be assigned issues; blocked tickets then
  resume with `amaru unblock <id>`. Triage is enough for the labels. Once it
  has a role, you can add `assignee: orp-bot` to `intake` in
  `coding-bot/project.yaml`. See [GitHub access](github-access.md).
- A token for the bot. For a fork onto a repository someone else owns, use a
  classic token with `public_repo`. A fine-grained token cannot open the
  pull request upstream.
- The `needs-implementation` label on this repository.

## Build

The image is built on the published base image,
`ghcr.io/amaru-oss/amaru-light:latest`:

```sh
podman build -f coding-bot/Containerfile -t localhost/amaru-light-amaru-light:dev coding-bot
```

To build on a local base image instead, run `make image` first and add
`--build-arg BASE_IMAGE=localhost/amaru-light:latest`.

The settings are copied into the image, so rebuild after you change
`coding-bot/amaru.yaml` or `coding-bot/project.yaml`.

Keep `RUST_TOOLCHAIN` in `coding-bot/Containerfile` on the release in
`rust-toolchain.toml` (and the Rust stage of the base `Containerfile`). If
they differ, rustup installs the pinned release on the first check, which
still works but takes time again each time the container is recreated.

## Run

```sh
export GH_TOKEN=<orp-bot token>
podman run -d --init --name amaru-light-amaru-light \
  -e GH_TOKEN \
  -p 127.0.0.1:8767:8767 \
  -v amaru-light-amaru-light:/home/mini-amaru:U \
  localhost/amaru-light-amaru-light:dev
```

`-e GH_TOKEN` with no value copies the token from the shell, so it is not in
the command line or the history, and the volume never stores it. Give it
again when you recreate the container. With Docker, drop `:U` and make sure
the volume is owned by uid 1000.

The web interface is at <http://127.0.0.1:8767>. It uses port 8767 so that
it can run next to other bots on 8765 and 8766. It is bound to 127.0.0.1; to
open it further, set `AMARU_WEB_TOKEN` as [Web interface](web-ui.md) says.

The volume holds the daemon state (`/home/mini-amaru/amaru`: database, clone
and worktrees), the Codex login and the cargo registry cache. Keep it when
you recreate the container.

## Sign Codex in

Once, on the web interface's Agents page, or:

```sh
podman exec -it amaru-light-amaru-light codex login --device-auth
podman exec amaru-light-amaru-light codex login status
```

Device login must be enabled for the ChatGPT account. The login stays in the
volume.

Optionally, give the bot's commits its identity before the first ticket:

```sh
podman exec amaru-light-amaru-light git config --global user.name orp-bot
podman exec amaru-light-amaru-light git config --global user.email <id>+orp-bot@users.noreply.github.com
```

## Verify

```sh
podman exec amaru-light-amaru-light /app/bin/amaru -c /config/amaru.yaml doctor
```

Every line must start with `ok`.

## Give it work

Add the `needs-implementation` label to an issue, or claim one:

```sh
podman exec amaru-light-amaru-light /app/bin/amaru -c /config/amaru.yaml claim 'amaru-oss/amaru-light#42'
```

Quote the reference: zsh treats `#` as a glob. Follow the work with
`podman logs -f amaru-light-amaru-light`, `amaru status`,
`amaru status --ticket <id>` and the web interface.

For a milestone, stop the container, run
`amaru drain amaru-oss/amaru-light --milestone "..."` in a
`podman run --rm -it` with the same volume, then start the container again.
Only one daemon may use a state directory.

When a ticket blocks, answer on the issue, then remove `amaru:blocked`, or
run `amaru unblock <id>` if the bot cannot label.

## What the image cannot run

`make check` (rustfmt, clippy and the tests, as CI runs them) works in the
image. These do not, because the image has no container runtime:

- `make image` and `make image-check`, and the CI `image` job.

Give the bot no issue that can only be verified by building the base image,
or check that part yourself on the pull request.
