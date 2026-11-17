# Source-documentation audit

This is the coverage checklist for the repository-wide source-documentation pass
in [KAN-167](https://jarettforzano.atlassian.net/browse/KAN-167). The normative
[standard lives in amagomori/.github](https://github.com/amagomori/.github/blob/main/docs/rust_documentation.md),
not in this checklist. Validation revisions, commands and results belong in the
implementation PR; a checked row means source review, not native execution.

## Coverage

The pass includes every existing first-party comment/docstring in the tracked
source directories below, including inline annotations and embedded probe
scripts. Files without comments were checked for non-obvious missing contracts,
not filled with descriptions of obvious code. Related accessors and fields share
their owning type/module's examples where useful.

| Reviewed | Directory or surface | Contracts and findings |
| --- | --- | --- |
| [x] | `crates/term-core/src/` | Crate orientation; exported types/fields/constants; parser state, physical rows versus logical lines, absolute positions, scrollback/reflow, grapheme widths, colors, selection, search, notifications, clipboard, command budgeting and rerun comparisons. Added examples and real error/panic conditions; corrected stale checksum and grapheme descriptions. |
| [x] | `crates/term-core/src/tests/`, inline test modules and `crates/term-core/src/images/tests.rs` / `kitty/tests.rs` | Fixture intent, protocol captures, row/link/grapheme invariants, resize expectations, benchmark units and chunking. Kept protocol bytes and golden snapshots unchanged. Added the FIFO-creation safety justification. |
| [x] | `crates/term-core/examples/` | Headless PTY conformance runner; polling, descriptor buffers, child/session ownership and fork/exec safety explanations. |
| [x] | `crates/term-core/tests/` and `tests/harness/` | Fuzz replay, allocator accounting, memory budgets and invariant callbacks. The allocator implementation documents delegation to `System` and the allocation/deallocation/reallocation obligations. |
| [x] | `crates/term-render/src/` | Public renderer/font/layout contracts, pixel/cell/byte units, bounds and ordering; atlas/damage/image lifetimes; font discovery and laziness; built-in/color glyphs and IME layout. Added shared runnable examples and one compile-only window/GPU example. |
| [x] | `crates/term-render/src/fallback.rs`, `present_shm.rs` | Reviewed Linux fontconfig ownership, borrowed charsets, optional-library checks, GL/HAL guards, shared-memory bounds and X-server read lifetime. Empty font sets now return before constructing a slice from a null array. |
| [x] | macOS branches of `crates/term-render/src/fallback.rs` and `renderer.rs` | Static review of Core Text create/copy ownership, retained collection elements, string/code-unit ranges, raw type casts and native-device access. Native macOS compilation/execution is a separate required QA gate. |
| [x] | `crates/term-app/src/` | Binary orientation, input/PTY queues, flow control, configuration/reloads, palettes, search/paste/close state, hover/drop/file-reference behavior, notifications, frame scheduling and benchmark logging. Moved misplaced allocator documentation and corrected stale command names. |
| [x] | Linux branches of `crates/term-app/src/` | X11 clocks and initialization, Wayland foreign-display/surface lifetimes, clipboard worker ownership, PTY descriptor borrowing/duplication, signals and process probes. |
| [x] | macOS branches of `crates/term-app/src/` | Static review of AppKit main-thread/receiver obligations, Objective-C callback ABIs and method replacement, Metal drawable feedback, Core Foundation bridging, process inspection, secure input and native tabs. Added private unsafe-function contracts and adjacent implementation justifications. Native execution remains required. |
| [x] | `crates/term-render/src/*.wgsl` | Rectangle/atlas coordinate units, glyph-mode sign convention, crop bounds and filtering. Preserved WGSL comment syntax. |
| [x] | `fuzz/fuzz_targets/` and `fuzz/Cargo.toml` | Parser/grid memory invariants and upstream-vte parity target; standalone nightly/sanitizer workspace boundary. Shared harness safety review is recorded above. |
| [x] | `.github/scripts/` including compare, latency, PTY/process-group, scroll-region, merge-queue and release tools | Python docstrings and inline comments, shell/Swift probes and embedded Rust/Vim/shell/configuration snippets. Normalized summary/detail separation, corrected stale source paths/commands and retained benchmark timing/noise limitations. |
| [x] | `.github/workflows/`, workspace/package manifests and comparison configuration | Scoped first-party lint opt-in, Linux/macOS documentation gates, separate benchmark target directories and release configuration comments. No upstream crate opts into the house lints. |
| [x] | `shell-integration/` | Bash/fish/zsh prompt/command boundaries, exit-status preservation, working-directory reports and shell-version limitations, using native shell comments. |
| [x] | `assets/generate_icons.py`, `crates/term-app/samples/` | Python generator/sample descriptions and pixel dimensions; reviewed generator sources without rewriting generated icons or raster fixtures. |
| [x] | `crates/term-app/terminfo/`, `themes/` and remaining first-party configuration | Corrected the terminfo header's stale claim that styled underlines were drawn only as single lines. Distinguished canceled advertised capabilities from the parser's supported escape sequences. Theme/configuration comments retain their native syntax. |

## Explicit correctness changes

The font-mapping concern was not resolved cosmetically. `font::read_cached` now
reads each font path into immutable, process-owned bytes, cached for the process
lifetime. External file truncation/replacement cannot invalidate a font parser's
borrowed slice. Successful snapshots deliberately remain cached by path; failed
reads do not. This replaces lazily faulted file mappings with full-file heap
storage for each font actually loaded, so fallback-heavy memory/startup cost must
be reviewed with the PR's performance evidence.

Regression tests retain and parse a snapshot through truncation/replacement and
retry a previously failed read. A separate fontconfig test covers an empty set
whose font-array pointer is null. These are visible correctness changes, not
silent behavior changes made to agree with prose.

## Exclusions and enforcement limits

- Preserved upstream comments, licenses and attribution in `vendor/vte/`, font
  assets and upstream-derived fixture material. The Trident-authored vte changes
  include the CSI fast path in `vendor/vte/src/lib.rs` and parity/recorder tests
  in `vendor/vte/tests/`; their comments were reviewed without restyling upstream
  material.
- Preserved generated lockfiles, images, captured protocol data, golden snapshots
  and packaging output. Dependency removal necessarily updates `Cargo.lock`;
  generated output is not prose-reformatted.
- `missing_docs` covers the two exported library APIs, not every private helper
  or binary item. The application still opts into the scoped Clippy policy.
- The narrow `missing_panics_doc` expectation on bundled-font construction covers
  validated embedded assets, not fallible caller input. No new blanket lint
  allowances, hidden public items or ignored doctests were introduced.
- Stable Clippy/rustdoc do not certify comment truth, example usefulness or unsafe
  invariants. Public and private strict rustdoc builds are separate to catch
  public-to-private links. Both supported operating systems must run the commands
  in the [development guide](development.md#source-documentation); Linux cannot
  validate macOS-only code. Hosted-check availability and native-platform results
  are reported separately in the PR.
