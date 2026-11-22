#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
config=false
web=false
command=
skip=false
for arg do
    case "$arg" in
        -c|--config|--config=*|-c?*) config=true ;;
        --web-dir|--web-dir=*) web=true ;;
    esac
done
for arg do
    if "$skip"; then skip=false; continue; fi
    case "$arg" in
        -c|--config) skip=true ;;
        --config=*|-c?*|-*) ;;
        *) command=$arg; break ;;
    esac
done
if ! "$config"; then set -- --config "$root/projects/example.yaml" "$@"; fi
if [ "$command" = serve ] && ! "$web"; then set -- "$@" --web-dir "$root/web"; fi
exec "$root/bin/mini-amaru" "$@"
