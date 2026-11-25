# Coding bot image

This folder holds the image and settings of Amaru Light's own coding bot.
The bot (`orp-bot`) runs Amaru Light with Codex on this repository's issues:
it changes, builds and checks the code with `make check`, pushes to its fork
`orp-bot/amaru-light` and opens pull requests here. It never merges.

- `Containerfile`: the project image, built on the Amaru Light base image.
- `amaru.yaml`: the daemon settings (state directory, web port 8767, Codex).
- `project.yaml`: this repository's settings (fork, intake label, checks,
  review).

To build and start it, see [The coding bot](../docs/coding-bot.md). The bot
picks open issues labelled `needs-implementation`.
