# Jira tickets

A project can take its tickets from Jira instead of GitHub issues. The code
and the pull requests stay on GitHub: the daemon reads the Jira issue, works
on it in the GitHub repository, and opens the pull request there, from the
bot's fork if the project has one. Review feedback and CI are handled on the
pull request exactly as for GitHub issues.

This fits projects such as Apache Camel, whose issues are on the ASF Jira
(`CAMEL-12345`) and whose code is on GitHub (`apache/camel`).

## Set it up

Add a `jira` block to the project file:

```yaml
repo: apache/camel
fork: my-bot/camel        # the bot pushes here and opens pull requests upstream
base_branch: main
agent: claude

jira:
  url: https://issues.apache.org/jira
  project: CAMEL
  # token_env: JIRA_TOKEN  # where the personal access token is, if any
  # jql: 'assignee = currentUser() AND labels = amaru'   # what `serve` picks up
  # comment: true

checks:
  - name: build
    run: mvn -B -q -pl components/camel-kafka -am install -DskipTests
```

A Jira project needs no `intake` block: without one its issues are claimed by
hand only. Then claim an issue by its key or its URL:

```sh
amaru -c amaru.yaml claim CAMEL-12345
amaru -c amaru.yaml claim https://issues.apache.org/jira/browse/CAMEL-12345 --now
```

The web page's claim box takes the same keys and URLs.

`amaru doctor` checks that `curl` works and that the Jira server answers, and
tells whether the token signs in.

## What changes for a Jira ticket

| | GitHub issue | Jira issue |
| --- | --- | --- |
| Ticket name in logs and on the page | `owner/name#42` | `CAMEL-12345` |
| Branch | `amaru/42-slug` | `amaru/CAMEL-12345-slug` |
| Pull request title | the issue title | `CAMEL-12345: <summary>` |
| Pull request body | `Closes #42` | `Fixes [CAMEL-12345](<issue URL>)` |
| Commit messages | the agent's choice | the agent is told to start each subject with `CAMEL-12345: ` |
| What the agent reads | the issue body | the description and every comment, refreshed when a blocked ticket resumes |
| Labels (`amaru:working`, `amaru:blocked`) | added when the bot may | never added; unblock with `amaru unblock <ticket>` or the web page |
| Comments (started, blocked, pull request opened) | always | only with a token and `comment: true`, and a refused comment does not stop the ticket |
| Dependencies | `Depends on #12`, GitHub's "blocked by" | Jira links "is blocked by" and "depends upon" to issues of the same project |
| Milestone for `amaru drain` | the milestone | the fix version: `amaru drain apache/camel --milestone 4.10.0` |

An issue is open while its status category is not "Done". Resolving or
closing it in Jira cancels the ticket, as closing a GitHub issue does.

## Tokens

Public issues, such as everything in the ASF Jira, are read without an
account. A token is only needed to comment, or to read a private project.

Create a personal access token in Jira (Profile → Personal Access Tokens) and
put it in the environment variable named by `token_env`, `JIRA_TOKEN` by
default. It is sent as a bearer token. For a server that takes a user name
and password instead, set `user:` and put the password or API token in that
variable.

The comments are posted as the token's owner. On the ASF Jira that is usually
your own account, since new accounts are not handed out freely, so leave
`comment` off if you would rather not have the bot post as you. The ASF's
GitHub integration already links a pull request whose title starts with the
key to the Jira issue.

The token never appears on a command line: the daemon passes it to `curl` on
its standard input.

## Picking up issues by itself

`amaru serve` picks up open issues of the Jira project that match any of:

- `intake.labels`, as `labels in (...)`,
- `intake.assignee`, as `assignee = "<name>"`,
- `jira.jql`, any JQL condition, for example
  `assignee = currentUser() AND labels = amaru` (`currentUser()` needs the
  token).

The daemon adds `project = CAMEL AND statusCategory != Done` itself. Issues
with a skip label are left alone, and priority labels, the fix version's
release date and age decide the order, as for GitHub.
