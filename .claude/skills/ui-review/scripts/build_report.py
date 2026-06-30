#!/usr/bin/env python3
"""Build a native thurbox-website docs page from captured screenshots + findings.

Reads a findings JSON (authored by Claude in the analyze phase) and the screenshots
dir, copies each PNG into website/assets/ui-review/, and writes a docs page at
website/docs/ui-review.html.

The page is an Eleventy *content* page: front matter (`layout: docs.njk`) plus a
bare body. It inherits the shared nav, the full `sidebar.njk` menu, and the CSS
chrome from the layout — so it never drifts from the other docs pages. The
review-specific widget styles live in `website/css/ui-review.css` (pulled in via
`extraCss`), not inline here.

Usage:
    build_report.py --findings findings.json --shots <screenshots-dir> --repo <repo-root>

The findings JSON schema:
{
  "title": "thurbox TUI — UI/UX Review",
  "version": "0.0.0-dev",
  "theme": "Doom",
  "generated_at": "2026-06-03 09:30 UTC",
  "recommendations": ["...", "..."],
  "screens": [
    {
      "file": "01-session-list.png",
      "label": "Session list + terminal (default view)",
      "keys": "launch",
      "findings": [
        {"severity": "warning", "lens": "visual",
         "title": "...", "desc": "...", "fix": "..."}
      ]
    }
  ]
}

severity ∈ {blocker, warning, nit}; lens ∈ {visual, usability, consistency, accessibility}.
"""
import argparse
import html
import json
import os
import shutil
import sys

SEV_ORDER = ["blocker", "warning", "nit"]
LENSES = ["visual", "usability", "consistency", "accessibility"]

# Map our severities/lenses to the site's CSS custom properties (variables.css).
SEV_VAR = {"blocker": "--red", "warning": "--yellow", "nit": "--blue"}
LENS_VAR = {
    "visual": "--purple",
    "usability": "--green",
    "consistency": "--yellow",
    "accessibility": "--blue",
}

def esc(s):
    return html.escape(str(s))


def yaml_q(s):
    # Single-quote a value for the YAML front matter (double any embedded quote).
    return "'" + str(s).replace("'", "''") + "'"


def build(data, shots_dir, repo_root):
    assets_dir = os.path.join(repo_root, "website", "assets", "ui-review")
    os.makedirs(assets_dir, exist_ok=True)
    # Clear stale screenshots so a re-run doesn't leave orphans.
    for old in os.listdir(assets_dir):
        if old.endswith(".png"):
            os.remove(os.path.join(assets_dir, old))

    sev_counts = {s: 0 for s in SEV_ORDER}
    lens_counts = {l: 0 for l in LENSES}
    for sc in data["screens"]:
        for f in sc.get("findings", []):
            sev_counts[f["severity"]] = sev_counts.get(f["severity"], 0) + 1
            lens_counts[f["lens"]] = lens_counts.get(f["lens"], 0) + 1
    total = sum(sev_counts.values())

    # Counts pills.
    pills = ""
    for s in SEV_ORDER:
        if sev_counts.get(s, 0):
            pills += (
                f'<span class="sev-pill"><span class="dot" style="background:var({SEV_VAR[s]})"></span>'
                f'{sev_counts[s]} {s}{"s" if sev_counts[s] != 1 else ""}</span>'
            )
    for l in LENSES:
        if lens_counts.get(l, 0):
            pills += (
                f'<span class="sev-pill"><span class="dot" style="background:var({LENS_VAR[l]})"></span>'
                f'{lens_counts[l]} {l}</span>'
            )

    recs = "".join(f"<li>{esc(r)}</li>" for r in data.get("recommendations", []))

    # On-this-page anchors, emitted as front-matter `onThisPage` items that the
    # shared sidebar.njk renders (summary + one per screen).
    on_page = "onThisPage:\n"
    on_page += "  - { id: 'summary', label: 'Summary' }\n"
    for i, sc in enumerate(data["screens"], 1):
        on_page += f"  - {{ id: 'screen-{i}', label: {yaml_q(sc['label'])} }}\n"
    on_page = on_page.rstrip("\n")

    # Cards.
    cards = ""
    for i, sc in enumerate(data["screens"], 1):
        src = os.path.join(shots_dir, sc["file"])
        if os.path.isfile(src):
            shutil.copy(src, os.path.join(assets_dir, sc["file"]))
            img = (
                f'<div class="review-shot"><img loading="lazy" alt="{esc(sc["label"])}" '
                f'src="../assets/ui-review/{esc(sc["file"])}"></div>'
            )
        else:
            img = '<div class="review-shot"><p style="color:var(--text-muted);padding:1rem">screenshot missing</p></div>'

        findings = sorted(sc.get("findings", []), key=lambda f: SEV_ORDER.index(f["severity"]))
        frows = ""
        for f in findings:
            sv, lv = SEV_VAR[f["severity"]], LENS_VAR[f["lens"]]
            frows += (
                '<div class="finding">'
                f'<span class="sev" style="color:var({sv});border-color:var({sv})">{esc(f["severity"])}</span>'
                '<div class="body">'
                f'<span class="ftitle">{esc(f["title"])}</span>'
                f'<span class="lens" style="color:var({lv});border-color:var({lv})">{esc(f["lens"])}</span>'
                f'<div class="fdesc">{esc(f["desc"])}</div>'
                f'<div class="ffix"><b>Fix:</b> {esc(f["fix"])}</div>'
                "</div></div>"
            )
        if not frows:
            frows = '<p style="color:var(--text-muted)">No findings.</p>'
        cards += (
            f'<div class="review-card" id="screen-{i}">'
            f'<h3>{esc(sc["label"])} <span class="keys">{esc(sc.get("keys", ""))}</span></h3>'
            f"{img}"
            f'<div class="review-findings">{frows}</div>'
            "</div>\n"
        )

    title = esc(data.get("title", "UI/UX Review"))
    # An Eleventy content page: front matter + bare body. The layout (docs.njk →
    # base.njk) supplies the head, nav, full sidebar, breadcrumbs, and CSS chrome.
    page = f"""---
layout: docs.njk
title: 'UI/UX Review — Thurbox Docs'
description: 'Generated UI/UX review of the thurbox TUI: screenshots of each screen with design, usability, consistency, and accessibility findings.'
currentPage: 'ui-review'
breadcrumb: 'UI/UX Review'
extraCss: ['ui-review.css']
{on_page}
---
<h1>{title}</h1>
<div class="review-meta">
  <span>thurbox {esc(data.get("version", "dev"))}</span>
  <span>theme: {esc(data.get("theme", "—"))}</span>
  <span>generated {esc(data.get("generated_at", ""))}</span>
</div>

<div class="info-box">
  <p>
    This page is generated by the <code>ui-review</code> skill: it drives the
    real TUI in an isolated sandbox, screenshots every screen, and critiques
    each through four lenses — visual design, usability, consistency, and
    accessibility. Re-run the skill to refresh it.
  </p>
</div>

<h2 id="summary">Summary — {total} findings across {len(data["screens"])} screens</h2>
<div class="sev-counts">{pills}</div>
<h3>Top recommendations</h3>
<ol>{recs}</ol>

{cards}"""

    out_path = os.path.join(repo_root, "website", "docs", "ui-review.html")
    with open(out_path, "w") as fh:
        fh.write(page)
    return out_path, total, len(data["screens"])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--findings", required=True)
    ap.add_argument("--shots", required=True)
    ap.add_argument("--repo", required=True)
    args = ap.parse_args()

    with open(args.findings) as fh:
        data = json.load(fh)

    out, total, screens = build(data, args.shots, args.repo)
    print(f"wrote {out} ({total} findings, {screens} screens)", file=sys.stderr)
    print(out)


if __name__ == "__main__":
    main()
