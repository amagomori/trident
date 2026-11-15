#!/usr/bin/env python3
"""Merge queue for amagomori/terminal: rebase each ready PR once, QA its merged tree, merge in order.

Each PR added to the queue is rebased once onto the queue head (main plus every PR ahead of it)
and its branch is pushed, so the queued head's tree is exactly what main will hold after its
squash merge. QA passes are recorded against that tree. Merging the front PR leaves main with
the same tree the next PR was rebased onto, so the next PR needs no rebase and its QA carries
over. Only a base move (main changed outside the queue, a PR ahead was dropped, or a queued
branch got new commits) rebases a PR again; QA is needed again only if its tree changed.

The queue lives in the remote ref refs/merge-queue/<base> (one queue.json), so every session
sees the same queue, and saves use --force-with-lease so two sessions can't overwrite each
other. Python 3 stdlib plus git and an authenticated gh."""
import argparse
import fnmatch
import json
import os
import shutil
import subprocess
import sys
import tempfile

BUILD_CONFIG = ("Cargo.toml", "*/Cargo.toml", "Cargo.lock", ".cargo/*", "rust-toolchain*",
                "build.rs", "*/build.rs", ".github/workflows/*")
MERGED_HISTORY = 50


class QueueError(Exception):
    pass


class Git:
    def __init__(self, cwd):
        self.cwd = cwd

    def run(self, *args, input=None, cwd=None):
        p = subprocess.run(["git", *args], cwd=cwd or self.cwd, input=input,
                           capture_output=True, text=True)
        if p.returncode:
            raise QueueError("git %s failed: %s" % (" ".join(args), p.stderr.strip()))
        return p.stdout.strip()

    def ok(self, *args):
        return subprocess.run(["git", *args], cwd=self.cwd, capture_output=True).returncode == 0

    def tree(self, rev):
        return self.run("rev-parse", rev + "^{tree}")

    def changed(self, a, b):
        out = self.run("diff", "--name-only", "--no-renames", a, b)
        return set(out.splitlines()) if out else set()


class GitHub:
    def __init__(self, repo):
        self.repo = repo

    def pr(self, number):
        out = subprocess.run(["gh", "api", "repos/%s/pulls/%d" % (self.repo, number)],
                             capture_output=True, text=True)
        if out.returncode:
            raise QueueError("gh api pulls/%d failed: %s" % (number, out.stderr.strip()))
        d = json.loads(out.stdout)
        return {"state": "merged" if d["merged"] else d["state"], "title": d["title"],
                "base": d["base"]["ref"], "head": d["head"]["sha"], "branch": d["head"]["ref"],
                "same_repo": (d["head"]["repo"] or {}).get("full_name") == self.repo}

    def merge(self, number, head, subject):
        out = subprocess.run(["gh", "pr", "merge", str(number), "-R", self.repo, "--squash",
                              "--match-head-commit", head, "--subject", subject],
                             capture_output=True, text=True)
        if out.returncode:
            raise QueueError("merging #%d failed: %s" % (number, out.stderr.strip()))


class Queue:
    def __init__(self, git, forge, remote="origin", base="main"):
        self.git, self.forge, self.remote, self.base = git, forge, remote, base
        self.ref = "refs/merge-queue/" + base
        self.state_commit = None
        self.entries, self.merged = [], []

    # state ---------------------------------------------------------------
    def load(self):
        self.fetch_main()
        if self.git.ok("fetch", "-q", self.remote, "+%s:%s" % (self.ref, self.ref)):
            self.state_commit = self.git.run("rev-parse", self.ref)
            state = json.loads(self.git.run("show", self.ref + ":queue.json"))
        else:
            self.state_commit, state = None, {}
        self.entries = state.get("entries", [])
        self.merged = state.get("merged", [])
        return self

    def save(self, message):
        body = json.dumps({"base": self.base, "entries": self.entries,
                           "merged": self.merged[-MERGED_HISTORY:]}, indent=2) + "\n"
        blob = self.git.run("hash-object", "-w", "--stdin", input=body)
        tree = self.git.run("mktree", input="100644 blob %s\tqueue.json\n" % blob)
        parent = ["-p", self.state_commit] if self.state_commit else []
        commit = self.git.run("commit-tree", tree, *parent, "-m", message)
        lease = "--force-with-lease=%s:%s" % (self.ref, self.state_commit or "")
        try:
            self.git.run("push", "-q", lease, self.remote, "%s:%s" % (commit, self.ref))
        except QueueError as e:
            raise QueueError("the queue changed under us (another session saved it); rerun: %s" % e)
        self.git.run("update-ref", self.ref, commit)
        self.state_commit = commit

    # helpers -------------------------------------------------------------
    def fetch_main(self):
        self.git.run("fetch", "-q", self.remote, "+refs/heads/%s:refs/remotes/%s/%s"
                     % (self.base, self.remote, self.base))

    def main_sha(self):
        return self.git.run("rev-parse", "refs/remotes/%s/%s" % (self.remote, self.base))

    def entry(self, number):
        for e in self.entries:
            if e["pr"] == number:
                return e
        raise QueueError("#%d is not in the queue" % number)

    def expected_onto(self, i):
        return self.entries[i - 1]["head"] if i else self.main_sha()

    def fetch_branch(self, branch):
        self.git.run("fetch", "-q", self.remote, "+refs/heads/%s:refs/merge-queue-pr/%s"
                     % (branch, branch))

    def rebase(self, head, onto, old_onto=None):
        """Replay head's own commits (those after old_onto, or not yet in onto) on top of onto.

        Commits whose changes are already on onto (a PR stacked on one queued ahead) drop out."""
        if old_onto and self.git.ok("merge-base", "--is-ancestor", old_onto, head):
            base = old_onto
        else:
            base = self.git.run("merge-base", head, onto)
        if base == onto:
            return head
        work = tempfile.mkdtemp(prefix="mq-")
        try:
            self.git.run("worktree", "add", "-q", "--detach", work, head)
            try:
                self.git.run("-c", "core.hooksPath=/dev/null", "rebase", "-q", "--empty=drop",
                             "--onto", onto, base, cwd=work)
            except QueueError:
                files = self.git.run("diff", "--name-only", "--diff-filter=U", cwd=work)
                self.git.ok("-C", work, "rebase", "--abort")
                raise QueueError("rebase conflicts in: %s" % (", ".join(files.split()) or "?"))
            new = self.git.run("rev-parse", "HEAD", cwd=work)
        finally:
            self.git.ok("worktree", "remove", "--force", work)
            shutil.rmtree(work, ignore_errors=True)
        if new == onto:
            raise QueueError("nothing left to merge: its changes are already on the queue head")
        return new

    def push(self, new, branch, old):
        if new != old:
            self.git.run("push", "-q", "--force-with-lease=refs/heads/%s:%s" % (branch, old),
                         self.remote, "%s:refs/heads/%s" % (new, branch))

    def ready(self, i):
        e = self.entries[i]
        return self.git.tree(self.expected_onto(i)) == e["onto_tree"] and e["qa_tree"] == e["tree"]

    # commands ------------------------------------------------------------
    def add(self, number, qa_tree=None):
        if any(e["pr"] == number for e in self.entries):
            raise QueueError("#%d is already queued" % number)
        info = self.forge.pr(number)
        if info["state"] != "open" or info["base"] != self.base:
            raise QueueError("#%d must be an open PR into %s" % (number, self.base))
        if not info["same_repo"]:
            raise QueueError("#%d comes from a fork; the queue can't push its branch" % number)
        self.fetch_main()
        self.fetch_branch(info["branch"])
        onto = self.expected_onto(len(self.entries))
        new = self.rebase(info["head"], onto)
        self.push(new, info["branch"], info["head"])
        e = {"pr": number, "title": info["title"], "branch": info["branch"],
             "original_head": info["head"], "onto": onto, "onto_tree": self.git.tree(onto),
             "head": new, "tree": self.git.tree(new), "qa_tree": qa_tree,
             "rebases": int(new != info["head"]), "requeued": None, "overlap": [],
             "build_config": False}
        self.entries.append(e)
        self.save("add #%d" % number)
        return e

    def qa_pass(self, number, tree=None):
        e = self.entry(number)
        if tree and tree != e["tree"]:
            raise QueueError("QA passed tree %s but #%d is queued at tree %s; QA that tree"
                             % (tree, number, e["tree"]))
        e["qa_tree"] = e["tree"]
        self.save("QA pass #%d at %s" % (number, e["tree"]))
        return e

    def drop(self, number, force=False):
        e = self.entry(number)
        if e.get("merging") and not force:
            raise QueueError("#%d is being merged; if no merge is running, rerun merge or use "
                             "drop --force" % number)
        self.entries.remove(e)
        self.save("drop #%d" % number)
        return e

    def sync(self):
        """Re-check every entry against its expected base; rebase only those whose base moved."""
        self.fetch_main()
        events, changed, i = [], False, 0
        while i < len(self.entries):
            e = self.entries[i]
            info = self.forge.pr(e["pr"])
            if info["state"] != "open" or info["base"] != self.base:
                why = ("PR is %s" % info["state"] if info["state"] != "open"
                       else "PR now targets %s" % info["base"])
                events.append(("removed", e, why))
                del self.entries[i]
                changed = True
                continue
            onto = self.expected_onto(i)
            onto_tree = self.git.tree(onto)
            if info["head"] != e["head"]:
                reason = "branch got new commits"
                self.fetch_branch(e["branch"])
            elif onto_tree != e["onto_tree"]:
                reason = "base moved"
            else:
                i += 1
                continue
            try:
                new = self.rebase(info["head"], onto, e["onto"])
                self.push(new, e["branch"], info["head"])
            except QueueError as err:
                events.append(("removed", e, str(err)))
                del self.entries[i]
                changed = True
                continue
            base_files = self.git.changed(e["onto_tree"], onto_tree)
            e.update(onto=onto, onto_tree=onto_tree, head=new, tree=self.git.tree(new),
                     rebases=e["rebases"] + int(new != info["head"]), requeued=reason,
                     overlap=sorted(base_files & self.git.changed(onto, new)),
                     build_config=any(fnmatch.fnmatch(f, p) for f in base_files for p in BUILD_CONFIG))
            events.append(("rebased", e, reason))
            changed = True
            i += 1
        if changed:
            self.save("sync")
        return events

    def merge(self, merge_all=False):
        merged, events = [], []
        while True:
            self.load()
            events += self.sync()
            if not self.entries or not self.ready(0):
                break
            e = self.entries[0]
            main = self.main_sha()
            if e["onto"] != main:
                # A squash leaves main at the queued base's tree but not its commit. Re-parent so
                # GitHub's merge base is main and the squash is exactly the QA'd tree.
                new = self.rebase(e["head"], main, e["onto"])
                if self.git.tree(new) != e["tree"]:
                    raise QueueError("re-parenting #%d onto %s changed its tree" % (e["pr"], main))
                self.push(new, e["branch"], e["head"])
                e.update(head=new, onto=main)
            e["merging"] = True
            self.save("merging #%d" % e["pr"])
            try:
                self.fetch_main()
                if self.main_sha() != main:
                    raise QueueError("%s moved while merging #%d; rerun" % (self.base, e["pr"]))
                self.forge.merge(e["pr"], e["head"], "%s (#%d)" % (e["title"], e["pr"]))
            except QueueError:
                e["merging"] = False
                self.save("merge #%d failed" % e["pr"])
                raise
            self.fetch_main()
            main_tree = self.git.tree(self.main_sha())
            self.entries.pop(0)
            self.merged.append({"pr": e["pr"], "head": e["head"], "tree": e["tree"],
                                "main": self.main_sha(), "main_tree": main_tree})
            self.save("merge #%d" % e["pr"])
            merged.append((e, main_tree))
            if not merge_all or main_tree != e["tree"]:
                break
        return merged, events


def describe(q, i):
    e = q.entries[i]
    base_ok = q.git.tree(q.expected_onto(i)) == e["onto_tree"]
    qa = "passed" if e["qa_tree"] == e["tree"] else "needed"
    state = "ready" if base_ok and qa == "passed" else ("needs sync" if not base_ok else "needs QA")
    line = "#%d %s: head %s tree %s base %s, QA %s, rebases %d -> %s" % (
        e["pr"], e["title"], e["head"][:12], e["tree"][:12], e["onto"][:12], qa,
        e["rebases"], state)
    if e["requeued"] and qa == "needed":
        scope = "full" if e["overlap"] or e["build_config"] else "quick (no file overlap)"
        line += "\n    re-QA %s after %s; overlap: %s; build config changed: %s" % (
            scope, e["requeued"], ", ".join(e["overlap"]) or "none",
            "yes" if e["build_config"] else "no")
    return line


def print_events(events):
    for kind, e, why in events:
        if kind == "removed":
            print("removed #%d from the queue: %s" % (e["pr"], why))
        else:
            print("rebased #%d (%s): frozen pair head %s tree %s base %s" % (
                e["pr"], why, e["head"], e["tree"], e["onto"]))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--repo", default="amagomori/terminal")
    ap.add_argument("--remote", default="origin")
    ap.add_argument("--base", default="main")
    ap.add_argument("-C", dest="cwd", default=os.getcwd(), help="git checkout to work in")
    sub = ap.add_subparsers(dest="cmd")
    sub.add_parser("status", help="list the queue in merge order").add_argument("--json", action="store_true")
    a = sub.add_parser("add", help="rebase a ready PR onto the queue head and queue it")
    a.add_argument("pr", type=int)
    a.add_argument("--qa-tree", help="tree QA already passed; carries over if the rebase keeps it")
    p = sub.add_parser("pass", help="record a QA pass for a queued PR's current tree")
    p.add_argument("pr", type=int)
    p.add_argument("--tree", help="tree QA tested; refused unless it is the queued tree")
    d = sub.add_parser("drop", help="take a PR out (QA fail or hold)")
    d.add_argument("pr", type=int)
    d.add_argument("--force", action="store_true", help="even if a merge of it was started")
    sub.add_parser("sync", help="rebase entries whose base moved")
    sub.add_parser("merge", help="squash-merge the front PR if ready").add_argument(
        "--all", action="store_true", help="keep merging while the front PR is ready")
    o = ap.parse_args(argv)
    q = Queue(Git(o.cwd), GitHub(o.repo), o.remote, o.base)
    try:
        q.load()
        if o.cmd == "add":
            q.add(o.pr, o.qa_tree)
            print(describe(q, len(q.entries) - 1))
        elif o.cmd == "pass":
            q.qa_pass(o.pr, o.tree)
            print(describe(q, q.entries.index(q.entry(o.pr))))
        elif o.cmd == "drop":
            q.drop(o.pr, o.force)
            print("dropped #%d; run sync to rebase the PRs behind it" % o.pr)
        elif o.cmd == "sync":
            print_events(q.sync())
        elif o.cmd == "merge":
            merged, events = q.merge(o.all)
            for e, main_tree in merged:
                note = "" if main_tree == e["tree"] else (
                    " but main is at tree %s: main moved during the merge; QA main" % main_tree)
                print("merged #%d at tree %s%s" % (e["pr"], e["tree"], note))
            print_events(events)
        if o.cmd in (None, "status") and getattr(o, "json", False):
            print(json.dumps(q.entries, indent=2))
        elif o.cmd != "add" and o.cmd != "pass":
            for i in range(len(q.entries)):
                print(describe(q, i))
            if not q.entries:
                print("queue is empty")
    except QueueError as e:
        sys.exit("mq: %s" % e)


if __name__ == "__main__":
    main()
