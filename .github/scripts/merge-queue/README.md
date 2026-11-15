# KAN-108 merge queue

`mq.py` merges ready amagomori/terminal PRs in order while rebasing each one once. A PR
entering the queue is rebased onto the queue head (main plus every PR ahead of it) and its
branch is pushed, so its head tree is what main will hold after its squash merge. QA passes are
recorded against that tree. Merging the front PR leaves main with exactly the tree the next PR
was rebased onto, so the next PR merges without another rebase and its QA carries over.

A PR is rebased again only when its base really moves: main changed outside the queue, a PR
ahead was dropped or rebased, or someone pushed to a queued branch. QA is needed again only if
that changes the PR's tree. For a re-QA, `status` says whether the base change touched any file
the PR touches or any build config (Cargo manifests and lockfile, `.cargo/`, toolchain,
`build.rs`, workflows): "full" re-QA if so, otherwise "quick" (merged-tree release PTY A/B).
A rebase conflict takes the PR out of the queue and leaves its branch as it was.

The queue is stored on the remote as `refs/merge-queue/main` (`queue.json`); every save is a
`--force-with-lease` push, so all sessions share one queue and can't overwrite each other.
Needs Python 3.9+, git and an authenticated `gh`; run it from a checkout of the repo.

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

Tests (local bare remote and a fake GitHub that squash-merges): `python3 -m unittest discover
-s .github/scripts/merge-queue`
