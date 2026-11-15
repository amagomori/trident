"""Tests for mq.py against a local bare remote and a fake GitHub that squash-merges like GitHub.

Run: python3 -m unittest discover -s .github/scripts/merge-queue"""
import os
import shutil
import subprocess
import tempfile
import unittest

import mq

ENV = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com", "GIT_COMMITTER_NAME": "t",
       "GIT_COMMITTER_EMAIL": "t@example.com", "GIT_CONFIG_GLOBAL": os.devnull,
       "GIT_CONFIG_NOSYSTEM": "1"}


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                          text=True).stdout.strip()


class FakeGitHub:
    """PRs over branches of the bare remote; merge squashes the PR's merged tree onto main."""

    def __init__(self, bare):
        self.bare, self.prs = bare, {}

    def pr(self, n):
        p = self.prs[n]
        head = git(self.bare, "rev-parse", "refs/heads/" + p["branch"])
        return {"state": p["state"], "title": p["title"], "base": p.get("base", "main"), "head": head,
                "branch": p["branch"], "same_repo": True}

    def merge(self, n, head, subject):
        assert self.pr(n)["head"] == head, "head moved"
        tree = git(self.bare, "merge-tree", "--write-tree", "main", head)
        commit = git(self.bare, "commit-tree", tree, "-p", "main", "-m", subject)
        git(self.bare, "update-ref", "refs/heads/main", commit)
        self.prs[n]["state"] = "merged"


class QueueTest(unittest.TestCase):
    def setUp(self):
        self._env = {k: os.environ.get(k) for k in ENV}
        os.environ.update(ENV)
        self.tmp = tempfile.mkdtemp(prefix="mq-test-")
        self.bare = os.path.join(self.tmp, "remote.git")
        git(self.tmp, "init", "-q", "--bare", "-b", "main", self.bare)
        self.dev = self.clone("dev")
        self.write({"a.txt": "a\n", "b.txt": "b\nx\nx\nx\nend\n", "c.txt": "c\n", "Cargo.toml": "[workspace]\n"})
        git(self.dev, "commit", "-qm", "init")
        git(self.dev, "push", "-q", "origin", "HEAD:main")
        self.gh = FakeGitHub(self.bare)
        self.q = self.queue(self.clone("qa"))

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)
        for k, v in self._env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def clone(self, name):
        path = os.path.join(self.tmp, name)
        git(self.tmp, "clone", "-q", self.bare, path)
        return path

    def queue(self, path):
        return mq.Queue(mq.Git(path), self.gh).load()

    def write(self, files):
        for name, text in files.items():
            with open(os.path.join(self.dev, name), "w") as f:
                f.write(text)
        git(self.dev, "add", "-A")

    def pr(self, n, files, start="origin/main"):
        git(self.dev, "fetch", "-q", "origin")
        git(self.dev, "checkout", "-q", "-B", "pr%d" % n, start)
        self.write(files)
        git(self.dev, "commit", "-qm", "change %d" % n)
        git(self.dev, "push", "-q", "-f", "origin", "pr%d" % n)
        self.gh.prs[n] = {"branch": "pr%d" % n, "title": "KAN-%d: Change" % n, "state": "open"}

    def push_main(self, files):
        git(self.dev, "fetch", "-q", "origin")
        git(self.dev, "checkout", "-q", "-B", "direct", "origin/main")
        self.write(files)
        git(self.dev, "commit", "-qm", "direct")
        git(self.dev, "push", "-q", "origin", "HEAD:main")

    def show(self, rev, path):
        return git(self.bare, "show", "%s:%s" % (rev, path))

    def pass_all(self):
        for e in list(self.q.entries):
            self.q.qa_pass(e["pr"])

    def test_add_rebases_onto_queue_head_once(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.pr(2, {"b.txt": "b2\n"})
        e1 = self.q.add(1)
        self.assertEqual(e1["rebases"], 0)
        e2 = self.q.add(2)
        self.assertEqual(e2["rebases"], 1)
        self.assertEqual(e2["onto"], e1["head"])
        self.assertEqual(self.gh.pr(2)["head"], e2["head"])
        self.assertEqual(self.show("pr2", "a.txt"), "a1")

    def test_merges_in_order_without_another_rebase(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.pr(2, {"b.txt": "b2\n"})
        self.pr(3, {"c.txt": "c3\n"})
        for n in (1, 2, 3):
            self.q.add(n)
        trees = [e["tree"] for e in self.q.entries]
        self.pass_all()
        merged, events = self.q.merge(merge_all=True)
        self.assertEqual(events, [])
        self.assertEqual([e["pr"] for e, _ in merged], [1, 2, 3])
        self.assertEqual([t for _, t in merged], trees)
        self.assertEqual([e["rebases"] for e, _ in merged], [0, 1, 1])
        self.assertEqual(git(self.bare, "log", "--format=%s", "-3", "main").splitlines(),
                         ["KAN-3: Change (#3)", "KAN-2: Change (#2)", "KAN-1: Change (#1)"])

    def test_merge_stops_at_first_pr_without_qa(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.pr(2, {"b.txt": "b2\n"})
        self.q.add(1)
        self.q.add(2)
        self.q.qa_pass(2)
        self.assertEqual(self.q.merge(merge_all=True), ([], []))
        self.q.qa_pass(1)
        self.q.merge()
        self.assertEqual([e["pr"] for e in self.q.entries], [2])
        self.assertTrue(self.q.ready(0))

    def test_pass_refuses_a_tree_other_than_the_queued_one(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.q.add(1)
        with self.assertRaises(mq.QueueError):
            self.q.qa_pass(1, tree=git(self.bare, "rev-parse", "main^{tree}"))
        self.assertIsNone(self.q.entries[0]["qa_tree"])

    def test_qa_tree_from_before_the_queue_carries_over_when_unchanged(self):
        self.pr(1, {"a.txt": "a1\n"})
        e = self.q.add(1, qa_tree=git(self.bare, "rev-parse", "pr1^{tree}"))
        self.assertTrue(self.q.ready(0))
        self.assertEqual(e["rebases"], 0)

    def test_drop_rebases_the_prs_behind_and_asks_for_qa_again(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.pr(2, {"b.txt": "b2\n"})
        self.pr(3, {"c.txt": "c3\n"})
        for n in (1, 2, 3):
            self.q.add(n)
        self.pass_all()
        self.q.drop(1)
        events = self.q.sync()
        self.assertEqual([(k, e["pr"]) for k, e, _ in events], [("rebased", 2), ("rebased", 3)])
        self.assertFalse(self.q.ready(0))
        self.assertEqual(self.q.merge(merge_all=True), ([], []))
        self.pass_all()
        merged, _ = self.q.merge(merge_all=True)
        self.assertEqual([e["pr"] for e, _ in merged], [2, 3])
        self.assertEqual(self.show("main", "a.txt"), "a")
        self.assertEqual(self.show("main", "c.txt"), "c3")

    def test_main_moving_outside_the_queue_reports_overlap_and_build_config(self):
        self.pr(1, {"a.txt": "a1\n", "b.txt": "b1\nx\nx\nx\nend\n"})
        self.q.add(1)
        self.q.qa_pass(1)
        self.push_main({"b.txt": "b\nx\nx\nx\nend2\n", "Cargo.toml": "[workspace]\nmembers = []\n"})
        self.assertFalse(self.queue(self.q.git.cwd).ready(0))
        (kind, e, why), = self.q.sync()
        self.assertEqual((kind, why), ("rebased", "base moved"))
        self.assertEqual(e["overlap"], ["b.txt"])
        self.assertTrue(e["build_config"])
        self.assertNotEqual(e["qa_tree"], e["tree"])
        self.assertIn("re-QA full", mq.describe(self.q, 0))

    def test_unrelated_main_move_is_a_quick_reqa(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.q.add(1)
        self.push_main({"c.txt": "c0\n"})
        (_, e, _), = self.q.sync()
        self.assertEqual(e["overlap"], [])
        self.assertFalse(e["build_config"])
        self.assertIn("re-QA quick", mq.describe(self.q, 0))

    def test_conflicting_pr_is_not_queued_and_its_branch_is_untouched(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.pr(2, {"a.txt": "a2\n"})
        self.q.add(1)
        before = self.gh.pr(2)["head"]
        with self.assertRaisesRegex(mq.QueueError, "a.txt"):
            self.q.add(2)
        self.assertEqual(self.gh.pr(2)["head"], before)
        self.assertEqual([e["pr"] for e in self.q.entries], [1])

    def test_conflict_after_a_base_move_takes_the_pr_out(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.pr(2, {"b.txt": "b2\n"})
        self.q.add(1)
        self.q.add(2)
        self.push_main({"a.txt": "a0\n"})
        events = self.q.sync()
        self.assertEqual(events[0][0], "removed")
        self.assertIn("a.txt", events[0][2])
        self.assertEqual([e["pr"] for e in self.q.entries], [2])
        self.assertEqual(self.q.entries[0]["onto"], self.q.main_sha())

    def test_new_commits_on_a_queued_branch_need_qa_again(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.q.add(1)
        self.q.qa_pass(1)
        git(self.dev, "checkout", "-q", "pr1")
        self.write({"a.txt": "a1b\n"})
        git(self.dev, "commit", "-qm", "fix")
        git(self.dev, "push", "-q", "origin", "pr1")
        (_, e, why), = self.q.sync()
        self.assertEqual(why, "branch got new commits")
        self.assertFalse(self.q.ready(0))

    def test_stacked_pr_drops_the_commits_already_queued(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.pr(2, {"b.txt": "b2\n"}, start="pr1")
        self.push_main({"c.txt": "c0\n"})
        self.q.add(1)
        e2 = self.q.add(2)
        self.assertEqual(git(self.bare, "rev-list", "--count", "%s..%s" % (e2["onto"], e2["head"])), "1")
        self.pass_all()
        merged, _ = self.q.merge(merge_all=True)
        self.assertEqual([e["pr"] for e, _ in merged], [1, 2])
        self.assertEqual(git(self.bare, "rev-parse", "main^{tree}"), e2["tree"])

    def test_stacked_edit_of_the_same_lines_merges_at_the_tested_tree(self):
        self.pr(1, {"a.txt": "A\n"})
        self.pr(2, {"a.txt": "a\n", "c.txt": "C\n"}, start="pr1")
        for n in (1, 2):
            self.q.add(n)
            self.q.qa_pass(n)
        tree2 = self.q.entry(2)["tree"]
        merged, _ = self.q.merge(merge_all=True)
        self.assertEqual([e["pr"] for e, _ in merged], [1, 2])
        self.assertEqual(git(self.bare, "rev-parse", "main^{tree}"), tree2)

    def test_retargeted_pr_is_removed(self):
        self.pr(1, {"a.txt": "A\n"})
        self.q.add(1)
        self.q.qa_pass(1)
        self.gh.prs[1]["base"] = "release"
        merged, events = self.q.merge()
        self.assertEqual(merged, [])
        self.assertEqual([(k, e["pr"]) for k, e, _ in events], [("removed", 1)])

    def test_pr_dropped_by_another_session_is_not_merged(self):
        self.pr(1, {"a.txt": "A\n"})
        self.q.add(1)
        self.q.qa_pass(1)
        other = self.queue(self.clone("other"))
        other.drop(1)
        merged, _ = self.q.merge()
        self.assertEqual(merged, [])
        self.assertEqual(self.gh.prs[1]["state"], "open")

    def test_a_pr_being_merged_is_not_dropped(self):
        self.pr(1, {"a.txt": "A\n"})
        self.q.add(1)
        self.q.entry(1)["merging"] = True
        self.q.save("merging #1")
        other = self.queue(self.clone("other"))
        with self.assertRaises(mq.QueueError):
            other.drop(1)
        other.drop(1, force=True)
        self.assertEqual(other.entries, [])

    def test_a_second_session_does_not_merge_a_pr_already_being_merged(self):
        self.pr(1, {"a.txt": "A\n"})
        self.q.add(1)
        self.q.qa_pass(1)
        other = self.queue(self.clone("other"))
        real = self.gh.merge

        def concurrent_merge(n, head, subject):
            with self.assertRaises(mq.QueueError):
                other.merge()
            real(n, head, subject)
        self.gh.merge = concurrent_merge
        merged, _ = self.q.merge()
        self.assertEqual([e["pr"] for e, _ in merged], [1])
        self.assertEqual([m["pr"] for m in self.queue(self.clone("check")).merged], [1])

    def test_merge_force_takes_over_a_stale_claim(self):
        self.pr(1, {"a.txt": "A\n"})
        self.q.add(1)
        self.q.qa_pass(1)
        self.q.entry(1)["merging"] = True
        self.q.save("merging #1")
        with self.assertRaises(mq.QueueError):
            self.q.merge()
        merged, _ = self.q.merge(force=True)
        self.assertEqual([e["pr"] for e, _ in merged], [1])

    def test_main_moving_during_the_merge_is_reported_and_stops(self):
        for n, f in ((1, "a.txt"), (2, "c.txt")):
            self.pr(n, {f: "%d\n" % n})
            self.q.add(n)
            self.q.qa_pass(n)
        real = self.gh.merge

        def racing_merge(n, head, subject):
            self.push_main({"b.txt": "raced\n"})
            real(n, head, subject)
        self.gh.merge = racing_merge
        merged, _ = self.q.merge(merge_all=True)
        self.assertEqual(len(merged), 1)
        e, main_tree = merged[0]
        self.assertNotEqual(main_tree, e["tree"])
        self.assertEqual([x["pr"] for x in self.q.entries], [2])

    def test_pr_merged_outside_the_queue_is_removed(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.pr(2, {"b.txt": "b2\n"})
        self.q.add(1)
        self.q.add(2)
        self.gh.merge(1, self.gh.pr(1)["head"], "outside")
        events = self.q.sync()
        self.assertEqual([(k, e["pr"]) for k, e, _ in events], [("removed", 1)])
        self.assertEqual(self.q.entries[0]["rebases"], 1)

    def test_queue_is_shared_and_saves_are_leased(self):
        self.pr(1, {"a.txt": "a1\n"})
        self.pr(2, {"b.txt": "b2\n"})
        other = self.queue(self.clone("other"))
        self.q.add(1)
        self.assertEqual([e["pr"] for e in self.queue(self.clone("third")).entries], [1])
        with self.assertRaisesRegex(mq.QueueError, "changed under us"):
            other.add(2)


    def test_cli_adds_passes_and_merges(self):
        import contextlib
        import io
        self.pr(1, {"a.txt": "a1\n"})
        orig, mq.GitHub = mq.GitHub, lambda repo: self.gh
        try:
            def run(*args):
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    mq.main(["-C", self.q.git.cwd, *args])
                return out.getvalue()
            self.assertIn("QA needed", run("add", "1"))
            tree = git(self.bare, "rev-parse", "pr1^{tree}")
            self.assertIn("-> ready", run("pass", "1", "--tree", tree))
            self.assertIn("merged #1 at tree " + tree, run("merge", "--all"))
            self.assertIn("queue is empty", run("status"))
        finally:
            mq.GitHub = orig

if __name__ == "__main__":
    unittest.main()
