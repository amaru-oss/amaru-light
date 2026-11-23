#!/bin/sh
# Container entrypoint: activate the ripwire agent skills for Claude Code and
# Codex in the runtime user's home, then start Amaru Light.
#
# The home directory is usually a volume, so the skills are activated at each
# start rather than at build time. The installer is idempotent: it links the
# skills shipped in the image and prunes the ones it created before. Set
# AMARU_RIPWIRE_SKILLS=0 to skip this step.
set -eu
skills=/usr/local/share/ripwire/skills/install.sh
if [ "${AMARU_RIPWIRE_SKILLS:-1}" != 0 ] && [ -x "$skills" ]; then
    for client in --claude --codex; do
        "$skills" "$client" >/dev/null 2>&1 \
            || echo "entrypoint: ripwire skills installer $client failed; continuing" >&2
    done
fi
exec /app/mini-amaru "$@"
