#!/bin/sh
# Integration check for a native package, including relocation and the bundled UI.
set -eu
archive=${1:?Usage: scripts/test-package.sh archive.tar.gz}
archive=$(CDPATH= cd -- "$(dirname -- "$archive")" && pwd)/$(basename -- "$archive")
work=$(mktemp -d "${TMPDIR:-/tmp}/mini-amaru-install-test.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
COPYFILE_DISABLE=1 tar -xzf "$archive" -C "$work"
source_dir=$(find "$work" -mindepth 1 -maxdepth 1 -type d | head -n 1)
destination="$work/installation with spaces"
"$source_dir/install.sh" "$destination"
if "$source_dir/install.sh" "$destination" >/dev/null 2>&1; then
    printf 'Installer overwrote an existing installation\n' >&2; exit 1
fi
if [ -e "$destination/data/mini-amaru.db" ]; then
    printf 'Package included a database\n' >&2; exit 1
fi
(cd / && "$destination/mini-amaru" jobs)
test -f "$destination/data/mini-amaru.db"
node - "$destination" <<'JS'
const { spawn } = require('node:child_process');
const path = require('node:path');
const assert = require('node:assert/strict');
const root = process.argv[2];
const child = spawn(path.join(root, 'mini-amaru'), ['serve', '--port', '0'], {cwd:'/', stdio:['ignore','pipe','pipe']});
let output = '';
let errors = '';
child.stderr.on('data', chunk => { errors += chunk; });
const timer = setTimeout(() => { console.error('Package server startup timed out', output, errors); child.kill(); process.exitCode = 1; }, 10000);
child.on('error', error => { clearTimeout(timer); console.error(error); process.exitCode = 1; });
child.stdout.on('data', async chunk => {
  output += chunk;
  const match = output.match(/http:\/\/127\.0\.0\.1:(\d+)/);
  if (!match || child.checked) return;
  child.checked = true;
  try {
    const base = `http://127.0.0.1:${match[1]}`;
    const page = await fetch(base);
    assert.equal(page.status, 200);
    assert.match(page.headers.get('content-type'), /text\/html/);
    const html = await page.text();
    const asset = html.match(/src="([^"]+\.js)"/);
    assert.ok(asset, 'bundled HTML must reference compiled JavaScript');
    const js = await fetch(new URL(asset[1], base));
    assert.equal(js.status, 200);
    assert.match(js.headers.get('content-type'), /javascript/);
    assert.equal((await fetch(`${base}/runs`)).status, 200);
    assert.equal((await (await fetch(`${base}/api/health`)).json()).ok, true);
    const bad = await fetch(`${base}/api/jobs`, {method:'POST',headers:{'Content-Type':'application/json',Origin:base},body:'{}'});
    assert.equal(bad.status, 400, 'same-origin POST must reach JSON validation');
    console.log('Packaged binary, installation, relocation, frontend and API passed.');
  } catch (error) { console.error(error, errors); process.exitCode = 1; }
  finally { clearTimeout(timer); child.kill(); }
});
child.on('exit', () => { if (!child.checked) { clearTimeout(timer); console.error('Server exited before startup', output, errors); process.exitCode = 1; } });
JS
