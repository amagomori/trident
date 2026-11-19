# Trident's winit patch

This is the published winit 0.30.13 crate with the X11 IME and macOS fullscreen
patches below. Original licenses and source provenance are retained. Cargo's registry
cache marker is omitted because this is a path dependency.

Upstream commit: `e9809ef54b18499bb4f2cac945719ecc2a61061b` (also recorded in
`.cargo_vcs_info.json`). Published crate SHA-256:
`a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`.
All other packaged source files compare byte-for-byte.

## X11 input methods

Native Fcitx/Mozc can deliver genuine nonempty XIM preedit DRAW/Update callbacks
without a preceding START. Upstream queues the Update but drops it in the event
processor while `is_composing` is false. Applications never learn composition
started, so they cannot cancel deferred cursor movement or render preedit.

Treat the first nonempty Update as composition start and deliver its Preedit.
Keep ignoring empty orphan Updates, preserve explicit START/END/DISABLED behavior,
and retain the existing keyboard suppression while composing. This is an X11-only
translation fix; no raw-event injection or application-specific input handling.

Run its native event-state regressions separately from the workspace:

```sh
cargo test --manifest-path vendor/winit/Cargo.toml --lib ime_translation
```

## macOS fullscreen

In `src/platform_impl/macos/{window,window_delegate}.rs`, the window subclass
overrides the public `setFrame:display:animate:` setter. When the key window
is entering or leaving native fullscreen, AppKit can call that setter to apply
a stale frame to each member of an unrelated tab group. Ignore those writes
before they resize the content view and PTY. The key window, its sibling tabs
and windows outside a transition still use the superclass implementation.

On a macOS desktop, run
`python3 .github/scripts/fullscreen_isolation.py target/release/trident`
from the repository root to exercise both directions.

Remove these patches when a winit release carrying equivalent fixes is adopted.
