#!/bin/sh
# Smoke test for the base image. It starts the image with its default
# command and checks the tools, the user, the ripwire skills, the API and
# the dashboard. Run it from the repository root, or use `make image-check`.
#
# Usage: scripts/test-image.sh [image]
# Environment: RUNTIME (podman or docker; default podman if installed).
set -eu
image=${1:-localhost/mini-amaru:latest}
if [ -z "${RUNTIME:-}" ]; then
    if command -v podman >/dev/null 2>&1; then RUNTIME=podman; else RUNTIME=docker; fi
fi
name="mini-amaru-image-check-$$"

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
for tool in "git --version" "gh --version" "ripwire --version" "/app/bin/mini-amaru --help"; do
    # shellcheck disable=SC2086
    "$RUNTIME" run --rm --entrypoint "" "$image" $tool >/dev/null \
        || fail "'$tool' does not run in the image"
done

"$RUNTIME" run -d --name "$name" -p 127.0.0.1::8765 "$image" >/dev/null
port=$("$RUNTIME" port "$name" 8765/tcp | head -n 1 | sed 's/.*://')
[ -n "$port" ] || fail "port 8765 is not published"
base="http://127.0.0.1:$port"

attempt=0
until curl -fsS "$base/api/health" >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 60 ] || fail "$base/api/health did not answer in 60 seconds"
    "$RUNTIME" container inspect -f '{{.State.Running}}' "$name" | grep -qx true \
        || fail "the container stopped"
    sleep 1
done

curl -fsS "$base/api/health" | grep -q '"ok":true' || fail "the health response is not ok"
curl -fsS "$base/api/jobs" >/dev/null || fail "GET /api/jobs failed"
curl -fsS "$base/" | grep -qi '<html' || fail "the dashboard is not served at /"

[ "$("$RUNTIME" exec "$name" id -un)" = mini-amaru ] || fail "the image does not run as mini-amaru"
"$RUNTIME" exec "$name" test -d /home/mini-amaru/.claude/skills \
    || fail "the entrypoint did not activate the ripwire skills for Claude Code"
"$RUNTIME" exec "$name" test -d /home/mini-amaru/.agents/skills \
    || fail "the entrypoint did not activate the ripwire skills for Codex"

printf 'OK: %s\n' "$image"
