# Failure focus (interaction prototype)

An isolated, read-only sketch of a Trident interaction: turn a noisy Cargo test transcript
into a list of failures, navigate with `[` / `]`, and return to the selected failure's
highlighted context in the full output. No commands are run, and nothing is added to
Trident's Rust application, event loop, renderer or release packages.

Open `index.html` directly in a modern browser. Start with **Focus failures**, select
the second failure, then **Show in full output**. The bundled output is fictional;
it is not a report of Trident failures. **Paste log** and **Open log file** accept real
local plain-text Cargo transcripts, limited to 1 MiB and 20,000 lines.

The page is deliberately plain, self-contained HTML rather than a hosted artifact kit:
there are no dependencies, external assets, requests, analytics or saved history.
Its content security policy blocks network connections. Log content is inserted as
text, never HTML. Copying requires the browser's clipboard permission; if unavailable,
the page tells you to select the text manually.

## Limits

- This is a browser interaction sketch, **not** a working terminal feature or a release commitment.
- Recognition targets Cargo's default `test NAME ... FAILED` and `---- NAME stdout ----`
  format, with optional suite boundaries and summary counts. ANSI colors are removed
  and CRLF is normalized; original line numbers and text context remain visible.
- Identical test names in different `Running` suites stay separate; completed summaries
  also separate stdout-only suites whose names were not captured. Partial logs may
  have a status line but no details, or details but no status line.
- Counts come from completed `test result:` summaries. A partial or unrecognized log
  is never declared successful. Compiler errors, nextest, doctest-specific output,
  arbitrary control sequences and interleaved parallel-suite logs are not fully parsed.
- It reads a completed snapshot, not live PTY output. It does not open source files,
  rerun tests, interpret terminal controls, or send logs to a model.
- A native version would need an explicit opt-in entry point, command-boundary tracking,
  bounded parsing away from the terminal's hot path, and performance/accessibility QA.
  Those are intentionally outside this prototype.

## Checks

With Node.js 18 or later, run:

```sh
node --test prototypes/failure-focus/parser.test.cjs
```

The tests exercise the same inline parser the page runs, without a build step or
browser dependencies. The page itself also works without Node.js.

## Authorship and isolation

All files in this directory were newly written by Devin for this prototype. No existing
code or other author's implementation is modified; the frozen prompt-click branches
are not used as a base or changed. The repository and this prototype remain private.
