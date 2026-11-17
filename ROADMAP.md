# Roadmap

This roadmap outlines Trident's milestones from the first usable build to a packaged 1.0 on
macOS and Linux. Track individual tickets on the
[KAN board in Jira](https://jarettforzano.atlassian.net/jira/software/projects/KAN/boards).

## Milestones

| Milestone | Goal | Epics |
| --- | --- | --- |
| v0.1 | Trident opens, runs your shell and renders on the GPU. | [Ship first usable terminal](https://jarettforzano.atlassian.net/browse/KAN-4) |
| v0.2 | Full-screen apps (vim, tmux, htop) behave as in xterm, with CI and tests in place. | [Set up engineering foundations](https://jarettforzano.atlassian.net/browse/KAN-5), [Reach xterm-level VT correctness](https://jarettforzano.atlassian.net/browse/KAN-10) |
| v0.3 | Rendering and everyday interactions meet their performance and usability targets. | [Make rendering fast and beautiful](https://jarettforzano.atlassian.net/browse/KAN-20), [Build daily-driver UX](https://jarettforzano.atlassian.net/browse/KAN-28) |
| v1.0 | Packages integrate natively with both platforms, are signed and install in one command. | [Integrate natively with macOS and Linux](https://jarettforzano.atlassian.net/browse/KAN-40), [Release and distribute 1.0](https://jarettforzano.atlassian.net/browse/KAN-44) |

## Labels

- Milestone labels are `v0.1`, `v0.2`, `v0.3` and `v1.0`.
- Area labels are `area-app` (window, PTY, input), `area-core` (VT parser and grid), `area-render` (GPU,
  fonts), `area-ux` (selection, config, tabs), `area-platform` (macOS and Linux integration),
  `area-infra` (CI, tests, benchmarks) and `area-release` (packaging, docs).

Tickets follow the team's [ticket format](https://github.com/amagomori/.github/blob/main/jira/ticket_format.md)
and [workflow](https://github.com/amagomori/.github/blob/main/jira/workflow.md). For the crate
layout, see [Development](docs/development.md#layout).
