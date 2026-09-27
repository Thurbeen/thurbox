"""Exercise the documented subset TUI rerun from an awkward checkout path."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


class RealTuiRerun(unittest.TestCase):
    def test_tmux_subset_without_other_muxes_from_spaced_path(self):
        repo = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="bench checkout with spaces ") as temp:
            root = Path(temp)
            shutil.copytree(Path(__file__).parent, root / "scripts" / "bench")
            bindir = root / "bin"
            bindir.mkdir()
            (bindir / "codex").symlink_to(shutil.which("codex"))
            output = root / "result.json"
            env = os.environ.copy()
            env["PATH"] = f"{bindir}:/usr/bin:/bin"
            env["BENCH_SOURCE_COMMIT"] = subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=repo, text=True
            ).strip()
            run = subprocess.run(
                [
                    sys.executable,
                    str(root / "scripts" / "bench" / "real_tui.py"),
                    "--hosts", "tmux", "--warmup", "0", "--reps", "1",
                    "--work", str(root / "work"), "--out", str(output),
                    "--thurbox-bin", str(root / "thurbox-not-installed"),
                ],
                cwd=repo,
                env=env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertEqual(run.returncode, 0, run.stdout[-1000:] + run.stderr[-2000:])
            data = json.loads(output.read_text())
            self.assertEqual(data["hosts"], ["tmux"])
            self.assertEqual([row["event"] for row in data["records"][0]["hooks"][:3]],
                             ["SessionStart", "UserPromptSubmit", "Stop"])
            self.assertNotIn("herdr", data["binary_sha256"])
            self.assertNotIn("rmux", data["binary_sha256"])


if __name__ == "__main__":
    unittest.main()
