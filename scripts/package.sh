#!/bin/sh
# Build an installable bundle without databases, credentials or dependency caches.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
CARGO=${CARGO:-cargo}
NPM=${NPM:-npm}
metadata=$("$CARGO" metadata --format-version 1 --no-deps --locked)
version=$(printf '%s' "$metadata" | node -e 'let s="";process.stdin.on("data",c=>s+=c);process.stdin.on("end",()=>console.log(JSON.parse(s).packages.find(p=>p.name==="mini-amaru").version))')
target_dir=$(printf '%s' "$metadata" | node -e 'let s="";process.stdin.on("data",c=>s+=c);process.stdin.on("end",()=>console.log(JSON.parse(s).target_directory))')
if [ -n "${TARGET:-}" ]; then
    target=$TARGET
    "$CARGO" build --release --locked --target "$target"
    binary="$target_dir/$target/release/mini-amaru"
else
    target=$(rustc -vV | sed -n 's/^host: //p')
    "$CARGO" build --release --locked
    binary="$target_dir/release/mini-amaru"
fi
case "$target" in
    *-apple-darwin|*-unknown-linux-gnu|*-unknown-linux-musl) ;;
    *) printf 'Unsupported package target: %s (Linux and macOS supported)\n' "$target" >&2; exit 1 ;;
esac
"$NPM" --prefix web ci --no-audit --no-fund
VITE_API_URL='' "$NPM" --prefix web run build
name="mini-amaru-$version-$target"
mkdir -p dist
stage=$(mktemp -d "${TMPDIR:-/tmp}/mini-amaru-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
bundle="$stage/$name"
mkdir -p "$bundle/bin" "$bundle/config" "$bundle/projects" "$bundle/skills" "$bundle/web" "$bundle/data/artifacts" "$bundle/data/worktrees" "$bundle/repos"
cp "$binary" "$bundle/bin/mini-amaru"
cp config/*.yaml "$bundle/config/"
cp projects/*.yaml "$bundle/projects/"
cp packaging/example.yaml "$bundle/projects/example.yaml"
cp -R skills/. "$bundle/skills/"
cp -R web/dist/. "$bundle/web/"
cp packaging/launch.sh "$bundle/mini-amaru"
cp packaging/install.sh "$bundle/install.sh"
cp packaging/INSTALL.md "$bundle/INSTALL.md"
cp README.md "$bundle/README.md"
printf '%s\n' "$target" > "$bundle/TARGET"
printf '%s\n' "$version" > "$bundle/VERSION"
chmod 755 "$bundle/bin/mini-amaru" "$bundle/mini-amaru" "$bundle/install.sh"
find "$bundle" -type f -name .DS_Store -delete
COPYFILE_DISABLE=1 tar -czf "dist/$name.tar.gz" -C "$stage" "$name"
if command -v sha256sum >/dev/null 2>&1; then
    (cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
else
    (cd dist && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
fi
printf '\nPackage: %s/dist/%s.tar.gz\nChecksum: %s/dist/%s.tar.gz.sha256\n' "$root" "$name" "$root" "$name"
