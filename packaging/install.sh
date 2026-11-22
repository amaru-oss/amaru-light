#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
destination=${1:-"$HOME/.local/share/mini-amaru"}
if [ "$#" -gt 1 ]; then printf 'Usage: %s [installation-directory]\n' "$0" >&2; exit 1; fi
# Verify the binary runs on this OS/CPU before creating the installation.
"$root/bin/mini-amaru" --version
if [ -e "$destination" ] && { [ ! -d "$destination" ] || [ -n "$(ls -A "$destination")" ]; }; then
    printf 'Destination must be a new or empty directory: %s\nExisting configuration and data will not be overwritten.\n' "$destination" >&2
    exit 1
fi
mkdir -p "$destination"
cp -R "$root/bin" "$root/config" "$root/projects" "$root/skills" "$root/web" "$destination/"
cp "$root/mini-amaru" "$root/README.md" "$root/INSTALL.md" "$root/TARGET" "$root/VERSION" "$destination/"
mkdir -p "$destination/data/artifacts" "$destination/data/worktrees" "$destination/repos"
probe="$destination/data/.write-test-$$"
if ! (umask 077; : > "$probe"); then
    printf 'Installation data directory is not writable: %s/data\n' "$destination" >&2
    exit 1
fi
rm "$probe"
printf '\nInstalled in %s\nEdit projects/example.yaml and config/providers.yaml there.\nStart: "%s/mini-amaru" serve\nWeb page: http://127.0.0.1:8765\n' "$destination" "$destination"
