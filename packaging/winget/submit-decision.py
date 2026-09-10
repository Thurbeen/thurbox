#!/usr/bin/env python3
"""The winget channel's two decisions, out of the workflow so they are testable.

Usage: submit-decision.py decide --throttle-days N [--now ISO8601] [PRS_JSON]
       submit-decision.py after-submit --exit-code N [OUTPUT_FILE]

`decide` reads `gh pr list --json number,state,createdAt,title` output (stdin by
default) and prints `{"should_submit": bool, "reason": str}`. Two things stop a
submission: a thurbox PR still **open** on winget-pkgs (submitting on top of it
is what accumulates the backlog its moderators complain about — wingetcreate has
no "update the pending PR" mode), and a last submission younger than
`--throttle-days`. At `--throttle-days 0` only the open-PR rule can gate, which
is the Chocolatey-parity cadence: attempt every release, let the moderation
queue itself set the pace.

`after-submit` reads a finished `wingetcreate submit` — its exit code, plus its
output (stdin by default) — and prints `{"opened": bool, "deferrable": bool,
"fail": bool, "reason": str}`. `deferrable` is the winget analog of the
Chocolatey job's 403/409 test: the channel pushed back, so warn, exit green and
retry next release. Everything else is a red job, including the stale-fork
failure the sync step ahead of `submit` exists to prevent.

`opened` is what gates the close-superseded-PRs step, and it is true only when
`submit` actually opened a PR. Gating that cleanup on the *pre-submit* decision
instead is a trap worth naming: a deferred submission exits green having opened
nothing, and cleanup would then close the pending thurbox PR on winget-pkgs and
put nothing in its place — leaving the channel with no PR at all, so the version
silently never ships. That is the failure this whole job exists to prevent, only
worse, which is why `opened` and `fail` are computed here and tested rather than
inferred in the workflow.

Exercised by winget.bats.
"""
import argparse
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

# Shapes that mean "the moderated channel is busy / already has this version",
# not "thurbox is broken". Kept deliberately narrow: anything unrecognised is
# worth a human's eyes, and the job-level `continue-on-error` already keeps a
# red winget job from reddening the release.
DEFERRABLE = [
    (r"(?i)\bAPI rate limit exceeded\b", "GitHub API rate limit"),
    (r"(?i)\bsecondary rate limit\b", "GitHub secondary rate limit"),
    (r"(?i)\b(429|abuse detection)\b", "GitHub throttling"),
    (r"(?i)already been submitted|already exists|has already been", "the version is already pending on winget-pkgs"),
]


def parse_iso(value: str) -> datetime:
    """Parse a GitHub timestamp (`2026-09-09T12:00:00Z`) as aware UTC."""
    return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)


def decide(prs, throttle_days: int, now: datetime) -> dict:
    open_prs = [p for p in prs if str(p.get("state", "")).upper() == "OPEN"]
    if open_prs:
        newest = max(open_prs, key=lambda p: parse_iso(p["createdAt"]))
        age = (now - parse_iso(newest["createdAt"])).total_seconds() / 86400
        return {
            "should_submit": False,
            "reason": (
                f"thurbox PR #{newest['number']} is still open on winget-pkgs "
                f"({age:.1f} days old); not stacking a second one on the moderation queue"
            ),
        }

    if not prs:
        return {
            "should_submit": True,
            "reason": "no prior thurbox PR on winget-pkgs — first submission",
        }

    newest = max(prs, key=lambda p: parse_iso(p["createdAt"]))
    age = (now - parse_iso(newest["createdAt"])).total_seconds() / 86400
    if age < throttle_days:
        return {
            "should_submit": False,
            "reason": f"last winget submission was {age:.1f} days ago (< {throttle_days}d throttle)",
        }
    return {
        "should_submit": True,
        "reason": f"last winget submission was {age:.1f} days ago (>= {throttle_days}d throttle)",
    }


def after_submit(exit_code: int, output: str) -> dict:
    if exit_code == 0:
        return {
            "opened": True,
            "deferrable": False,
            "fail": False,
            "reason": "wingetcreate submit opened a pull request on winget-pkgs",
        }
    for pattern, why in DEFERRABLE:
        if re.search(pattern, output):
            return {"opened": False, "deferrable": True, "fail": False, "reason": why}
    return {
        "opened": False,
        "deferrable": False,
        "fail": True,
        "reason": "not a known moderated-channel rejection",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    d = sub.add_parser("decide")
    d.add_argument("prs", nargs="?", default="-", help="gh pr list JSON, or - for stdin")
    d.add_argument("--throttle-days", type=int, required=True)
    d.add_argument("--now", default=None, help="ISO-8601 instant to age against (default: now)")

    a = sub.add_parser("after-submit")
    a.add_argument("output", nargs="?", default="-", help="wingetcreate output, or - for stdin")
    a.add_argument("--exit-code", type=int, required=True, help="wingetcreate submit's exit code")

    args = parser.parse_args()
    path = args.prs if args.command == "decide" else args.output
    source = sys.stdin.read() if path == "-" else Path(path).read_text(encoding="utf-8")

    if args.command == "decide":
        if args.throttle_days < 0:
            print("error: --throttle-days must be >= 0", file=sys.stderr)
            return 2
        now = parse_iso(args.now) if args.now else datetime.now(timezone.utc)
        result = decide(json.loads(source), args.throttle_days, now)
    else:
        result = after_submit(args.exit_code, source)

    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
