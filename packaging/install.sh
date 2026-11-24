#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
destination=${1:-"$HOME/.local/share/amaru"}
if [ "$#" -gt 1 ]; then printf 'Usage: %s [installation-directory]\n' "$0" >&2; exit 1; fi
# Verify the binary runs on this OS/CPU before creating the installation.
"$root/bin/amaru" --version
if [ -e "$destination" ] && { [ ! -d "$destination" ] || [ -n "$(ls -A "$destination")" ]; }; then
    printf 'Destination must be a new or empty directory: %s\nExisting settings and data will not be overwritten.\n' "$destination" >&2
    exit 1
fi
mkdir -p "$destination"
cp -R "$root/bin" "$root/config" "$destination/"
cp "$root/amaru" "$root/README.md" "$root/INSTALL.md" "$root/TARGET" "$root/VERSION" "$destination/"
mkdir -p "$destination/config/data"
probe="$destination/config/data/.write-test-$$"
if ! (umask 077; : > "$probe"); then
    printf 'The data directory is not writable: %s/config/data\n' "$destination" >&2
    exit 1
fi
rm "$probe"
printf '\nInstalled in %s\nEdit config/amaru.yaml and config/project.yaml there.\nCheck: "%s/amaru" doctor\nStart: "%s/amaru" serve\n' "$destination" "$destination" "$destination"
