# Web interface

`amaru serve` also serves a small web interface, at
<http://127.0.0.1:8765> by default. It shows what the daemon is doing and
offers the same actions as the command line:

- **Tickets**: every ticket with its state, pull request and last change.
  The counters filter the list: in progress, awaiting review, needing you,
  finished. Paste an issue (`owner/name#42` or its URL) to claim it, as
  `amaru claim` does.
- **A ticket**: its links (issue, pull request, branch), the reason it is
  blocked, and a timeline of everything the daemon and the agent did: state
  changes, the agent's messages and commands, failed checks, errors. The
  timeline follows new events as they arrive. **Unblock** and **Cancel** do
  what `amaru unblock` and `amaru cancel` do.
- **Projects**: the settings of each project file, read only. Edit the files
  and restart the daemon to change them.
- **Agents**: whether each agent CLI is signed in, and signing it in. See
  [Signing agents in](#signing-agents-in).

The page is built into the `amaru` binary; there is nothing else to install
or run. `amaru drain` and `amaru claim --now` do not start it.

## Settings

```yaml
web:
  enabled: true          # set to false to run without the web interface
  listen: 127.0.0.1:8765
```

The daemon fails at startup when it cannot listen on that address, for
example when another daemon already uses the port. Give each daemon its own
port.

In a container, listen on all interfaces inside the container and publish
the port on the host's loopback:

```yaml
web:
  listen: 0.0.0.0:8765
```

```sh
podman run ... -p 127.0.0.1:8765:8765 ...
```

## Access and the token

Anyone who can reach the port can claim, unblock and cancel tickets, and
sign the agent CLIs in. With the default `127.0.0.1`, that is only the
users of the same machine.

To reach the page from elsewhere, prefer an SSH tunnel
(`ssh -L 8765:127.0.0.1:8765 my-server`) over opening the port. If you do
open it, set a token in the daemon's environment:

```sh
export AMARU_WEB_TOKEN="$(openssl rand -hex 24)"
```

The page then asks for the token once and keeps it in the browser's local
storage. API clients send it as `Authorization: Bearer <token>`. The daemon
logs a warning when it listens on a non-loopback address without a token.

The page does not use HTTPS itself; put a reverse proxy in front of it if
the token has to cross a network you do not trust.

## Signing agents in

Amaru does not ship the agent CLIs or their accounts. The Agents page runs
each CLI's own sign-in command as the user that runs the daemon, so the
login lands where the agent will look for it: in that user's home
directory, or in the directory that `CODEX_HOME` or `CLAUDE_CONFIG_DIR`
point to, set through the agent's `env`.

For each agent, the page shows:

- **Signed in** or **not signed in**, from the CLI's status command. **Check
  again** runs it again; **Status output** shows what it printed.
- **Sign in** starts the sign-in command and shows its output as it comes.
  Open the link it prints in any browser, on any device.
  - Codex (`codex login --device-auth`) prints a link and a one-time code.
    Enter the code on that page; the sign-in finishes by itself. Device
    login must be enabled for the ChatGPT account or workspace.
  - Claude Code (`claude auth login`) prints a link. After you approve, the
    page shows a code: paste it into the field under the output and press
    **Send**.
- **Use an API key** signs in with a key instead, where the CLI supports
  it. It runs `codex login --with-api-key` for Codex, writes the key to
  its input and closes it, as `echo "$KEY" | codex login --with-api-key`
  would. The key field is masked. Amaru does not store, log or show the
  key; the CLI keeps its login as usual. Claude Code has no such command:
  give it `ANTHROPIC_API_KEY` through the agent's `env` or the container's
  environment instead.
- **Stop** ends a sign-in that is no longer needed. A sign-in still running
  after 15 minutes is stopped.

Other CLIs, or other ways to sign in, are a matter of settings: `login`,
`login_with_key` and `login_status` hold the arguments given to the
agent's `command`; `login_with_key` fits any CLI that reads a key on
stdin until the end of input. For example:

```yaml
agents:
  claude:
    driver: claude
    command: claude
    login: [auth, login, --console]     # Anthropic Console instead of a subscription
  codex:
    driver: codex
    command: codex
    env:
      CODEX_HOME: /home/mini-amaru/.codex-bot
    login: []                           # no Sign in button for this one
```

The status command counts as signed in when it exits with code 0. The
output of a sign-in is kept in the daemon's memory only, and only for the
latest sign-in of each agent.

In a container, keep the home directory on a volume (see
[Build a project container](building-project-containers.md)), or the login
is lost when the container is recreated.

## The API

The page uses a JSON API that scripts can use too. Requests that change
something are `POST` requests and must carry an `X-Amaru-Action` header
(any value), which a page on another site cannot send.

| Request | Does |
| --- | --- |
| `GET /api/health` | `{"ok": true}`, without a token. |
| `GET /api/info` | The version, the daemon settings and the projects. |
| `GET /api/tickets?limit=200` | Tickets, newest first. |
| `POST /api/tickets` `{"issue": "owner/name#42"}` | Claim an issue. |
| `GET /api/tickets/{id}` | One ticket. |
| `GET /api/tickets/{id}/events?after={event id}` | Its events, oldest first. |
| `POST /api/tickets/{id}/unblock` | Let a blocked ticket continue. |
| `POST /api/tickets/{id}/cancel` | Cancel a ticket. |
| `GET /api/agents` | The agents and their latest sign-in. |
| `GET /api/agents/{name}/status` | Run the agent's status command. |
| `POST /api/agents/{name}/login` | Start a sign-in. With `{"key": "..."}`, the API-key sign-in. |
| `GET /api/agents/{name}/login` | The sign-in's output and outcome. |
| `POST /api/agents/{name}/login/input` `{"text": "..."}` | Send a line to the sign-in. |
| `POST /api/agents/{name}/login/cancel` | Stop the sign-in. |

```sh
curl -s -X POST -H 'X-Amaru-Action: 1' -H 'Content-Type: application/json' \
  -d '{"issue": "my-user/my-project#42"}' http://127.0.0.1:8765/api/tickets
```
