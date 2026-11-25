#!/bin/sh
# Integration check for a native package: install, relocation and first start.
set -eu
archive=${1:?Usage: scripts/test-package.sh archive.tar.gz}
archive=$(CDPATH= cd -- "$(dirname -- "$archive")" && pwd)/$(basename -- "$archive")
work=$(mktemp -d "${TMPDIR:-/tmp}/amaru-install-test.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
COPYFILE_DISABLE=1 tar -xzf "$archive" -C "$work"
source_dir=$(find "$work" -mindepth 1 -maxdepth 1 -type d | head -n 1)
destination="$work/installation with spaces"
"$source_dir/install.sh" "$destination"
if "$source_dir/install.sh" "$destination" >/dev/null 2>&1; then
    printf 'Installer overwrote an existing installation\n' >&2; exit 1
fi
if [ -e "$destination/config/data/amaru.db" ]; then
    printf 'Package included a database\n' >&2; exit 1
fi
(cd / && "$destination/amaru" status)
test -f "$destination/config/data/amaru.db"
printf 'Packaged binary, installation, relocation and first start passed.\n'
