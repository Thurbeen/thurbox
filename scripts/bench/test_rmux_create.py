"""End-to-end smoke probe for the standalone RMUX benchmark host."""

import json
import os
import subprocess
import sys
import tempfile
import unittest


class RmuxCreateProbe(unittest.TestCase):
    def test_standalone_rmux_creates_agent_in_isolated_sandbox(self):
        cache = os.path.expanduser("~/.cache/b5")
        os.makedirs(cache, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=cache) as work:
            result = subprocess.run(
                [
                    sys.executable,
                    os.path.join(os.path.dirname(__file__), "run.py"),
                    "--hosts",
                    "rmux",
                    "--scenarios",
                    "create",
                    "--quick",
                    "--reps",
                    "1",
                    "--warmup",
                    "0",
                    "--work",
                    work,
                    "--out",
                    os.path.join(work, "results"),
                ],
                capture_output=True,
                text=True,
                check=False,
                timeout=90,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            with open(os.path.join(work, "results", "results.json")) as data:
                raw = json.load(data)
            rows = [row for row in raw["records"] if row["scenario"] == "create"]
            self.assertEqual([row["variant"] for row in rows], ["N=1", "N=5"])
            self.assertTrue(
                all(
                    row["host"] == "rmux" and row["metrics"]["total_ms"] > 0
                    for row in rows
                )
            )


if __name__ == "__main__":
    unittest.main()
