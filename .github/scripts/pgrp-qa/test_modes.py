"""Headless checks for the workload gate (the input-mode check needs a desktop)."""

import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import modes


class FixtureTests(unittest.TestCase):
    def test_receipt_matches_output_and_termination_for_every_scenario(self):
        for scenario in modes.SCENARIOS:
            with self.subTest(scenario=scenario), tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp)
                with (work / "output").open("wb") as output:
                    proc = subprocess.run([sys.executable, "-c", modes.FIXTURE, scenario],
                                          stdout=output, cwd=work, timeout=10)
                rc = proc.returncode
                self.assertEqual(rc, -signal.SIGKILL if scenario.endswith("kill") else 0)
                (work / "fixture.status").write_text(str(128 - rc if rc < 0 else rc))
                result = modes.fixture_evidence(work, scenario)
                self.assertNotIn("fixture_error", result)
                self.assertEqual(result["fixture_receipt"], f"{scenario} {(work / 'output').stat().st_size}")

    def test_invalid_evidence(self):
        for status, receipt in [
            ("0", None), (None, "flood-tail 20971560"), ("0", "garbled"),
            ("0", "flood-tail 40"), ("137", "flood-tail 20971560"), ("0", "flood-exit 20971560"),
        ]:
            with self.subTest(status=status, receipt=receipt), tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp)
                for name, value in [("fixture.status", status), ("fixture.receipt", receipt)]:
                    if value is not None:
                        (work / name).write_text(value)
                evidence = modes.fixture_evidence(work, "flood-tail")
                self.assertIn("fixture_error", evidence)

    def test_missing_fixture_fails_even_when_mode_leaks_are_report_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = ["modes", "--main", "main", "--head", "head", "--runs", "1",
                    "--scenarios", "flood-tail", "--out", tmp]
            evidence = modes.fixture_evidence(Path(tmp), "flood-tail")
            with patch.object(sys, "argv", args), patch.object(modes, "Mac" if modes.MAC else "X11"), \
                    patch.object(modes, "one_run", return_value=evidence), self.assertRaises(SystemExit) as exit:
                modes.main()
            self.assertEqual(exit.exception.code, 1)


if __name__ == "__main__":
    unittest.main()
