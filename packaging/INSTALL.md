# Install Amaru Light

This archive contains the `amaru` executable, example settings and an
installer. Rust is not needed to run it. Use an archive that matches your
operating system and CPU (see `TARGET`). Linux GNU builds also need a glibc
at least as new as the build machine's; for older systems build a musl
target.

Install Git, the GitHub CLI (`gh`) and your agent CLI (`claude` or `codex`)
on the target machine and sign in to the agent as the user who will run the
daemon. SQLite is compiled into the executable.

```sh
./install.sh
# Or choose a new, writable installation directory:
./install.sh "$HOME/bot/amaru"
```

No sudo is needed. The installer refuses to overwrite a nonempty directory.
The `amaru` launcher in the installation uses `config/amaru.yaml` unless you
pass `--config`.

1. Edit `config/project.yaml`: the repository, an optional fork, the intake
   labels or assignee, the check commands and the review settings.
2. Edit `config/amaru.yaml`: the agent CLIs and their arguments. The state
   (`config/data/` by default) holds the database, a mirror clone of each
   repository and the ticket worktrees.
3. Export the bot account's token as `GH_TOKEN`, then:

```sh
./amaru doctor
./amaru serve
```

`doctor` checks the settings, Git, `gh auth status` and each agent CLI.
`./amaru status` lists the tickets.
