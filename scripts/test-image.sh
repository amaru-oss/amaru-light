#!/bin/sh
# Smoke test for the base image. It checks the tools and the user, then
# starts the daemon with the example settings and checks that it keeps
# running, that the entrypoint activated the ripwire skills, and that it
# created its database. Run it from the repository root, or use
# `make image-check`.
#
# Usage: scripts/test-image.sh [image]
# Environment: RUNTIME (podman or docker; default podman if installed).
set -eu
image=${1:-localhost/amaru-light:latest}
if [ -z "${RUNTIME:-}" ]; then
    if command -v podman >/dev/null 2>&1; then RUNTIME=podman; else RUNTIME=docker; fi
fi
name="amaru-image-check-$$"

fail() {
    printf 'test-image: %s\n' "$1" >&2
    "$RUNTIME" logs "$name" >&2 2>&1 || true
    exit 1
}

cleanup() {
    "$RUNTIME" rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

# The tools that the image must contain, run without the entrypoint.
for tool in "git --version" "gh --version" "ripwire --version" "/app/bin/amaru --version"; do
    # shellcheck disable=SC2086
    "$RUNTIME" run --rm --entrypoint "" "$image" $tool >/dev/null \
        || fail "'$tool' does not run in the image"
done

# Start the daemon on the example settings, with the state on /data. With no
# token every poll fails, which the daemon must survive.
"$RUNTIME" run -d --name "$name" --entrypoint /bin/sh "$image" -c '
    sed "s|^state_dir: .*|state_dir: /data|" /app/examples/daemon.yaml > /config/amaru.yaml
    cp /app/examples/project.yaml /config/project.yaml
    exec /app/entrypoint --config /config/amaru.yaml serve' >/dev/null

attempt=0
until "$RUNTIME" exec "$name" test -f /data/amaru.db 2>/dev/null; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 30 ] || fail "the daemon did not create /data/amaru.db in 30 seconds"
    "$RUNTIME" container inspect -f '{{.State.Running}}' "$name" | grep -qx true \
        || fail "the container stopped"
    sleep 1
done
sleep 3
"$RUNTIME" container inspect -f '{{.State.Running}}' "$name" | grep -qx true \
    || fail "the daemon exited"

[ "$("$RUNTIME" exec "$name" id -un)" = mini-amaru ] || fail "the image does not run as mini-amaru"
"$RUNTIME" exec "$name" test -d /home/mini-amaru/.claude/skills \
    || fail "the entrypoint did not activate the ripwire skills for Claude Code"
"$RUNTIME" exec "$name" test -d /home/mini-amaru/.agents/skills \
    || fail "the entrypoint did not activate the ripwire skills for Codex"

printf 'OK: %s\n' "$image"
