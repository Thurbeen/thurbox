#!/usr/bin/env python3
"""Build a native thurbox-website docs page from captured screenshots + findings.

Reads a findings JSON (authored by Claude in the analyze phase) and the screenshots
dir, copies each PNG into website/assets/ui-review/, and writes a docs page at
website/docs/ui-review.html that reuses the site's nav / sidebar / CSS chrome.

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

NAV = """    <nav class="nav scrolled" id="nav">
      <!-- prettier-ignore -->
      <a href="../index.html" class="nav-brand">thur<span>box</span></a>
      <button class="hamburger" id="hamburger" aria-label="Toggle menu">
        <span></span>
        <span></span>
        <span></span>
      </button>
      <ul class="nav-links" id="nav-links">
        <li><a href="../index.html#features">Features</a></li>
        <li><a href="../index.html#installation">Install</a></li>
        <li><a href="index.html" style="color: var(--text-primary)">Docs</a></li>
        <li>
          <a
            href="https://github.com/Thurbeen/thurbox"
            class="github-link"
            target="_blank"
            rel="noopener"
          >
            <svg class="icon-github" viewBox="0 0 16 16">
              <path
                d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"
              />
            </svg>
            GitHub
          </a>
        </li>
      </ul>
    </nav>"""

SIDEBAR = """        <aside class="docs-sidebar" id="docs-sidebar">
          <h4>Getting Started</h4>
          <ul>
            <li><a href="installation.html">Installation</a></li>
            <li><a href="features.html">Features</a></li>
            <li><a href="keybindings.html">Keybindings</a></li>
          </ul>
          <h4>Reference</h4>
          <ul>
            <li><a href="architecture.html">Architecture</a></li>
          </ul>
          <h4>Developer</h4>
          <ul>
            <li><a href="ui-review.html" class="current-page">UI/UX Review</a></li>
          </ul>
        </aside>"""


def esc(s):
    return html.escape(str(s))


def page_css():
    # Page-local styles for the review-specific widgets, expressed in the site's vars.
    return """    <style>
      .review-meta { color: var(--text-secondary); font-family: var(--font-mono);
        font-size: 0.85rem; margin-bottom: var(--space-lg); }
      .review-meta span { margin-right: var(--space-lg); }
      .sev-counts { display: flex; flex-wrap: wrap; gap: var(--space-sm);
        margin: var(--space-md) 0 var(--space-lg); }
      .sev-pill { display: inline-flex; align-items: center; gap: 0.4rem;
        padding: 0.25rem 0.75rem; border: 1px solid var(--border);
        font-family: var(--font-mono); font-size: 0.8rem; }
      .sev-pill .dot { width: 9px; height: 9px; border-radius: 50%; }
      .review-card { border: 1px solid var(--border); background: var(--bg-secondary);
        margin-bottom: var(--space-2xl); }
      .review-card > h3 { margin: 0; padding: var(--space-md) var(--space-lg);
        border-bottom: 1px solid var(--border); font-size: 1.05rem; }
      .review-card > h3 .keys { color: var(--text-muted); font-family: var(--font-mono);
        font-size: 0.8rem; font-weight: 400; margin-left: 0.5rem; }
      .review-shot { background: #000; text-align: center; border-bottom: 1px solid var(--border); }
      .review-shot img { max-width: 100%; height: auto; display: block; margin: 0 auto; }
      .review-findings { padding: var(--space-sm) var(--space-lg) var(--space-md); }
      .finding { display: flex; gap: var(--space-md); align-items: flex-start;
        padding: var(--space-md) 0; border-top: 1px solid var(--border); }
      .finding:first-child { border-top: none; }
      .finding .sev { flex-shrink: 0; font-family: var(--font-mono); font-size: 0.7rem;
        text-transform: uppercase; letter-spacing: 0.04em; padding: 0.2rem 0.5rem;
        border: 1px solid; min-width: 70px; text-align: center; }
      .finding .body { flex: 1; }
      .finding .body .ftitle { font-weight: 600; }
      .finding .body .lens { font-family: var(--font-mono); font-size: 0.7rem;
        padding: 0.1rem 0.5rem; border: 1px solid; margin-left: 0.5rem; }
      .finding .body .fdesc { color: var(--text-secondary); margin: 0.3rem 0; font-size: 0.9rem; }
      .finding .body .ffix { color: var(--text-muted); font-size: 0.85rem; }
      .finding .body .ffix b { color: var(--text-secondary); }
    </style>"""


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

    # On-this-page anchors.
    on_page = '<h4>On This Page</h4>\n          <ul>\n'
    on_page += '            <li><a href="#summary">Summary</a></li>\n'
    for i, sc in enumerate(data["screens"], 1):
        on_page += f'            <li><a href="#screen-{i}">{esc(sc["label"])}</a></li>\n'
    on_page += "          </ul>"

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
    page = f"""<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>UI/UX Review — Thurbox Docs</title>
    <meta
      name="description"
      content="Generated UI/UX review of the thurbox TUI: screenshots of each screen with design, usability, consistency, and accessibility findings."
    />
    <link rel="icon" href="../assets/favicon.svg" type="image/svg+xml" />
    <link rel="preconnect" href="https://fonts.googleapis.com" />
    <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
    <link
      rel="stylesheet"
      href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600&family=JetBrains+Mono:wght@400;600;700&family=Press+Start+2P&display=swap"
    />
    <link rel="stylesheet" href="../css/variables.css" />
    <link rel="stylesheet" href="../css/base.css" />
    <link rel="stylesheet" href="../css/layout.css" />
    <link rel="stylesheet" href="../css/components.css" />
    <link rel="stylesheet" href="../css/docs.css" />
{page_css()}
  </head>
  <body>
{NAV}

    <div class="docs-page">
      <div class="docs-layout">
{SIDEBAR.replace("          <h4>On This Page</h4>", "")}

        <button class="sidebar-toggle" id="sidebar-toggle" aria-label="Toggle sidebar">
          &#9776;
        </button>

        <main class="docs-content">
          <div class="breadcrumbs">
            <a href="../index.html">Home</a>
            <span class="separator">/</span>
            <a href="index.html">Docs</a>
            <span class="separator">/</span>
            <span class="current">UI/UX Review</span>
          </div>

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

{cards}
        </main>
      </div>
    </div>

    <script src="../js/main.js"></script>
  </body>
</html>
"""

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
