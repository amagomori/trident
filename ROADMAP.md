# Roadmap

Trident's path from the first usable build to a packaged 1.0 on macOS and Linux. Tickets and their
status live on the [KAN board in Jira](https://jarettforzano.atlassian.net/jira/software/projects/KAN/boards),
not here.

## Milestones

| Milestone | Goal | Epics |
| --- | --- | --- |
| v0.1 | Opens, runs your shell and renders on the GPU. | [Ship first usable terminal](https://jarettforzano.atlassian.net/browse/KAN-4) |
| v0.2 | Full-screen apps (vim, tmux, htop) behave as in xterm, with CI and tests in place. | [Set up engineering foundations](https://jarettforzano.atlassian.net/browse/KAN-5), [Reach xterm-level VT correctness](https://jarettforzano.atlassian.net/browse/KAN-10) |
| v0.3 | Fast, good-looking and usable all day. | [Make rendering fast and beautiful](https://jarettforzano.atlassian.net/browse/KAN-20), [Build daily-driver UX](https://jarettforzano.atlassian.net/browse/KAN-28) |
| v1.0 | Native on both platforms, signed and installable in one command. | [Integrate natively with macOS and Linux](https://jarettforzano.atlassian.net/browse/KAN-40), [Release and distribute 1.0](https://jarettforzano.atlassian.net/browse/KAN-44) |

## Labels

- Milestones: `v0.1`, `v0.2`, `v0.3`, `v1.0`.
- Areas: `area-app` (window, PTY, input), `area-core` (VT parser and grid), `area-render` (GPU,
  fonts), `area-ux` (selection, config, tabs), `area-platform` (macOS and Linux integration),
  `area-infra` (CI, tests, benchmarks) and `area-release` (packaging, docs).

Tickets follow the team's [ticket format](https://github.com/amagomori/.github/blob/main/jira/ticket_format.md)
and [workflow](https://github.com/amagomori/.github/blob/main/jira/workflow.md). For the crate
layout, see [Development](docs/development.md#layout).
