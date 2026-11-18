# Merge queue

`mq.py` merges ready Trident PRs in order and rebases them on entry or when their base changes.
Run it from a repository checkout with Python 3.9 or later, git and an authenticated `gh`.
The `--repo` option defaults to `amagomori/terminal`, which GitHub redirects to
`amagomori/trident`.

## How it works

- When a PR enters the queue, `mq.py` rebases it onto the queue head (main plus every PR ahead of
  it) and pushes the branch. Its head tree is then what main holds after its squash merge.
- QA passes are recorded against that tree. Merging the front PR leaves main at the tree the next PR
  was rebased onto, so the next PR merges without another rebase and keeps its QA pass.
- A PR is rebased again only when its base changes. This happens if main changed outside the
  queue, a PR ahead was dropped or rebased, or someone pushed to a queued branch.
- After such a rebase, `status` says which re-QA the PR needs. It's "full" if the base change
  touched any file the PR touches or any build config (Cargo manifests and lockfile, `.cargo/`,
  toolchain, `build.rs`, workflows). Otherwise it's "quick", a release PTY A/B of the merged tree.
- A rebase conflict takes the PR out of the queue and leaves its branch unchanged.
- The queue is stored on the remote as `refs/merge-queue/main` in `queue.json`. Every save is a
  `--force-with-lease` push, so all sessions share one queue and can't overwrite each other.

## Commands

```sh
q=.github/scripts/merge-queue/mq.py
python3 $q add 81                 # rebase onto the queue head, push, print head/tree/base
python3 $q add 82 --qa-tree <t>   # QA already passed tree <t>; kept if the rebase keeps it
python3 $q pass 81 --tree <t>     # record a QA pass; refused unless <t> is the queued tree
python3 $q drop 81 [--force]      # QA fail or hold: take it out
python3 $q sync                   # rebase PRs whose base moved; prints new frozen pairs
python3 $q merge --all [--force]  # squash-merge while the front is ready; --force takes over a dead claim
python3 $q status [--json]
```

## Test

The tests use a local bare remote and a fake GitHub that squash-merges:

```sh
python3 -m unittest discover -s .github/scripts/merge-queue
```
