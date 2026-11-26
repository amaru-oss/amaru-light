#!/bin/sh
# Run amaru with the settings file of this installation unless one is given.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# The prompt guard, amaru-guard, is next to amaru.
PATH="$root/bin:$PATH"
export PATH
for arg do
    case "$arg" in
        -c|--config|--config=*|-c?*) exec "$root/bin/amaru" "$@" ;;
    esac
done
exec "$root/bin/amaru" --config "$root/config/amaru.yaml" "$@"
