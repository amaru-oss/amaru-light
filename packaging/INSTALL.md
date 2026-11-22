# Install Mini-Amaru

This archive contains the Rust executable, compiled web page, YAML configuration,
project templates, and skills. Rust, Cargo, Node.js and npm are not needed to run it.
Use an archive matching your operating system and CPU (see TARGET). Linux GNU
builds also require a compatible glibc; build on the oldest Linux system you intend
to support, or build for a musl target with its required linker/toolchain installed.

Install Git, GitHub CLI (`gh`) and your chosen agent CLI separately on the target
machine, then authenticate them as the user who will run Mini-Amaru. SQLite is
compiled into the executable. Agent CLIs, credentials, repositories and existing
job databases are not included in the archive.

After extracting:

```sh
./install.sh
# Or choose a new, writable installation directory:
./install.sh "$HOME/bot/mini-amaru"
```

No sudo is needed. The installer refuses to overwrite a nonempty installation and
checks that its data directory is writable. You can also run directly from the
extracted directory. Keep bin/, config/, projects/, skills/ and web/ together.

1. Edit `projects/example.yaml`: set your repository, checkout path, check command
   and policies. When a job or loop starts, a missing or empty checkout is cloned
   automatically from `project.repository` using your authenticated `gh` CLI.
   Existing nonempty directories are preserved. Included project YAML
   files from the build machine are additional examples; adjust their paths.
2. Edit `config/providers.yaml`, `models.yaml` and `routing.yaml` for your installed
   tools/models. In particular, remove or adjust the Codex `CODEX_HOME` override
   if you do not use `~/.codex-plus` on this machine.

If planning reports no available model, run `./mini-amaru doctor -c projects/example.yaml`
with the same project config and shell you use for `serve`. The bundled routing
tries Claude first for planning, then Codex if Claude is unavailable. A provider
command must be executable on that shell's PATH or use an absolute path in
`config/providers.yaml`. The model must also be allowed by the project.
3. Run the launcher from the installation directory:

```sh
./mini-amaru doctor
./mini-amaru serve
# Or choose your project and another port:
./mini-amaru serve -c projects/midnight-saci.yaml --port 9000
```

The web page and API are both at **http://127.0.0.1:8765** by default. The launcher
adds the bundled frontend automatically; `bin/mini-amaru` is the raw executable.
The launcher works from any working directory with its default project. Explicit
relative `-c` paths are relative to your current working directory. Use an absolute
configuration path if launching it elsewhere. `serve --web-dir PATH` overrides the
bundled frontend.

For a browser on another computer, keep the server local and use an SSH tunnel:

```sh
ssh -L 8765:127.0.0.1:8765 coding-bot@thyone
```

Then open http://127.0.0.1:8765 on the browser computer. The API has no authentication;
do not expose it directly to the network.

SQLite needs write access to both its database and the containing data/ directory
(to create journal files). If startup says "unable to open database file":

```sh
ls -ld . data data/mini-amaru.db
mkdir -p data/artifacts data/worktrees
touch data/.write-test && rm data/.write-test
```

Ensure the installation and existing database belong to the account running it;
use a user-owned installation directory. `l` is a shell alias on some machines;
`ls` is the standard command.

To upgrade, stop the old server, back up its configuration/data, install into a new
directory, then copy your configuration and data there. Do not copy an active
SQLite database without using SQLite's backup facilities or stopping the server.
