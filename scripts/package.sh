#!/bin/sh
# Build an installable bundle without databases, credentials or checkouts.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
CARGO=${CARGO:-cargo}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
target_dir=${CARGO_TARGET_DIR:-target}
if [ -n "${TARGET:-}" ]; then
    target=$TARGET
    "$CARGO" build --release --locked --bin amaru --target "$target"
    binary="$target_dir/$target/release/amaru"
else
    target=$(rustc -vV | sed -n 's/^host: //p')
    "$CARGO" build --release --locked --bin amaru
    binary="$target_dir/release/amaru"
fi
case "$target" in
    *-apple-darwin|*-unknown-linux-gnu|*-unknown-linux-musl) ;;
    *) printf 'Unsupported package target: %s (Linux and macOS supported)\n' "$target" >&2; exit 1 ;;
esac
name="amaru-$version-$target"
mkdir -p dist
stage=$(mktemp -d "${TMPDIR:-/tmp}/amaru-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
bundle="$stage/$name"
mkdir -p "$bundle/bin" "$bundle/config"
cp "$binary" "$bundle/bin/amaru"
cp examples/daemon.yaml "$bundle/config/amaru.yaml"
cp examples/project.yaml "$bundle/config/project.yaml"
cp packaging/launch.sh "$bundle/amaru"
cp packaging/install.sh "$bundle/install.sh"
cp packaging/INSTALL.md "$bundle/INSTALL.md"
cp README.md "$bundle/README.md"
printf '%s\n' "$target" > "$bundle/TARGET"
printf '%s\n' "$version" > "$bundle/VERSION"
chmod 755 "$bundle/bin/amaru" "$bundle/amaru" "$bundle/install.sh"
COPYFILE_DISABLE=1 tar -czf "dist/$name.tar.gz" -C "$stage" "$name"
if command -v sha256sum >/dev/null 2>&1; then
    (cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
else
    (cd dist && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
fi
printf '\nPackage: %s/dist/%s.tar.gz\nChecksum: %s/dist/%s.tar.gz.sha256\n' "$root" "$name" "$root" "$name"
