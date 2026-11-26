# GitHub access

Amaru Light works on GitHub as a bot account of its own, so that its
comments, branches and pull requests are clearly apart from yours. It does
everything through two tools that read the bot's token:

- `gh`, for issues, labels, comments and pull requests. It reads the token
  from `GH_TOKEN`.
- `git`, for cloning, fetching and pushing over HTTPS. Git needs to be told
  to get its credentials from `gh`; see [Git credentials](#git-credentials).

The daemon never merges and only pushes to the branches it created.

## Pushing to the repository or to a fork

Choose one per project:

- **Direct.** Leave `fork` out. The bot pushes branches to `repo` and
  opens pull requests from them. The bot needs write access to `repo`.
- **Fork.** Set `fork: my-bot/name` to the bot's fork. The bot pushes
  branches to the fork and opens pull requests on `repo` from
  `my-bot:branch`. The bot needs no write access to `repo`.

The daemon sets this up itself, never the agent, before it prepares a
worktree, before every agent turn and before every push:

- With `fork`, it makes sure the fork exists and creates it when it is
  missing: in the bot's own account, or in the organization named by the
  fork's owner. A repository with that name that is not a fork of `repo`
  stops the ticket, as does a fork GitHub keeps under another name (it
  says which name to set).
- In the clone and in each worktree, `origin` is where branches are pushed
  (the fork, or `repo` without one) and `upstream` is `repo`, which ticket
  branches start from. Anything else, such as a remote URL or push URL the
  agent changed, is put back. Clones made by earlier versions, with `origin`
  for `repo` and `fork` for the fork, are renamed to this layout.
- When the daemon cannot make sure of the fork or the remotes, the ticket
  stops with the reason before the agent runs. A GitHub error that may pass
  is retried a few times first.

The agent is told not to push or change the remotes.

With a fork, a pull request for a dependent issue (stacked on another
ticket's branch) targets the base branch instead of the other branch,
because GitHub cannot open a pull request against a branch of a fork. Its
description names the pull request it builds on, and its diff includes
that pull request's commits until it merges.

## What the bot needs

| Action | Needs |
| --- | --- |
| Read issues and pull requests, clone | Read access (any public repository). |
| Comment on issues, open pull requests from a fork, reply to review comments, react to reviews and comments | A signed-in account; no role on a public repository. |
| Push branches | Write access to the repository it pushes to: `repo` (direct) or the fork. |
| Create its fork when it is missing | A token that can fork: a classic token with `public_repo` (`repo` for a private repository), and, to fork into an organization, the right to create repositories there. |
| Add and remove the `amaru:working` and `amaru:blocked` labels | Triage or write access to `repo`. |
| Be assigned issues (`intake.assignee`) | Access to `repo`: GitHub only lets you assign collaborators. |

On a repository owned by a user, not an organization, the only
collaborator role is write. Triage exists only in organizations.

### Without write access to the upstream repository

This is the common case for a bot that contributes through a fork. The
daemon works, with these differences:

- It cannot label issues. It logs `cannot add the amaru:working label`,
  records a `LABEL_SKIPPED` event and goes on.
- If GitHub refuses a reaction (for example a fine-grained token without
  access to the upstream repository), it logs `cannot react to <id>`,
  records a `REACTION_SKIPPED` event and goes on. It does not retry that
  reaction.
- When a ticket blocks, its comment on the issue says to run
  `amaru unblock <ticket>`; removing a label does nothing, because there is
  none.
- Use `intake.labels` to hand it work: you add the `amaru` label (or your
  own) to an issue. `intake.assignee` does not work, because the bot
  cannot be assigned.

## The token

Give the bot a personal access token and pass it as `GH_TOKEN`. In a
container, use `-e GH_TOKEN` so that the token stays out of the command
line; the volume does not store it.

- **Direct.** A fine-grained token for `repo` with *Contents*, *Issues*
  and *Pull requests* set to read and write.
- **Fork, upstream owned by someone else.** A classic token with the
  `public_repo` scope (`repo` if the repository is private). A fine-grained
  token cannot act on a repository owned by another account that has not
  granted it access, so it cannot open the upstream pull request.

`amaru doctor` runs `gh auth status` to check that the token is valid. It
does not check each permission: the first ticket does.

## Git credentials

`gh` reads `GH_TOKEN`, but Git does not. Configure Git once to ask `gh`
for HTTPS credentials:

```sh
gh auth setup-git
```

In an image, you can set the same thing system-wide at build time, so that
no step is needed at run time:

```dockerfile
RUN git config --system credential.https://github.com.helper '' \
    && git config --system --add credential.https://github.com.helper '!/usr/bin/gh auth git-credential'
```

Without it, the clone works for a public repository but the first push
fails.

## Commit author

The daemon sets the author `Amaru Light <amaru-light@users.noreply.github.com>`
on its clone when Git has no identity configured at the first sync. To
commit as the bot, set a global identity before the daemon starts its
first ticket:

```sh
git config --global user.name my-bot
git config --global user.email <id>+my-bot@users.noreply.github.com
```

If the clone already exists, set the identity in it instead, under
`<state_dir>/repos/<owner>/<name>/mirror`.
