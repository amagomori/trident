# Trident's winit patch

This is the published winit 0.30.13 crate, with local Linux patches for X11 IME translation
and Wayland/X11 file drops. Original licenses and source provenance are retained. Cargo's registry
cache marker is omitted because this is a path dependency.

Upstream commit: `e9809ef54b18499bb4f2cac945719ecc2a61061b` (also recorded in
`.cargo_vcs_info.json`). Published crate SHA-256:
`a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`.

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

The stable upstream 0.30.13 release and 0.30 maintenance branch still contain the
guard. Vendoring keeps this narrow patch reproducible without an unpinned fork or
requiring build-time mutations of Cargo's registry cache. Remove this copy when
an upstream release carrying the fix is adopted.

Native Wayland seats bind a data device for copy-only local file drops. Both Linux
backends split URI lists on LF or CRLF before decoding filename bytes. Run
`cargo test --manifest-path vendor/winit/Cargo.toml --lib uri_list` for parser coverage.
