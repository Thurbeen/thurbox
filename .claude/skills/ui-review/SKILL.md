---
name: ui-review
description: Capture screenshots of the thurbox TUI across its screens and panels, then generate an HTML report with UI/UX feedback. Run from the thurbox repo.
user-invocable: true
allowed-tools: Read, Write, Bash, Glob, Skill
---

# ui-review — screenshot the thurbox TUI and critique its UI/UX

This skill drives the **real** thurbox TUI inside an isolated, throwaway
environment, takes a PNG screenshot of each major screen/panel, then has Claude
*look at* every screenshot and write a self-contained **HTML report** with UI/UX
feedback.

It reuses the proven isolation + seeding model from `scripts/demo/record.sh`, but
swaps VHS video output for VHS `Screenshot` directives. Nothing it does touches your
real thurbox sessions, tmux server, or agent accounts — everything runs against the
`thurbox-dev` dev build in a `mktemp` sandbox that is torn down on exit.

**Run this from the thurbox repo root** (or any subdir of it). The output lands in
`target/ui-review/` (git-ignored).

## Requirements

`vhs` (+ `ffmpeg` + `ttyd`), `sqlite3`, `cargo`, `git`, `tmux`. The capture script
preflights these and fails fast with a clear message if any are missing. Real agent
CLIs (`claude`/`codex`/`antigravity`/`opencode`) are **optional** — if none are installed
the script registers a stub agent so every panel still renders.

## Phase 1 — Capture

Resolve the skill directory and run the capture script. It builds the dev binaries,
sets up the sandbox, seeds sessions/tasks/an automation, and runs VHS to emit one PNG
per UI state plus a `manifest.json` describing them.

```bash
SKILL_DIR="$(cd "$(dirname "$(readlink -f .claude/skills/ui-review/SKILL.md)")" && pwd)"
# Default output dir is <repo>/target/ui-review/screenshots. Override with arg 1.
# Optional flags: --theme <thurbox-theme> (default doom), --width N, --height N.
"$SKILL_DIR/scripts/capture.sh"
```

When the user names a specific theme to review (e.g. "review the Tokyo Night theme"),
pass `--theme "Tokyo Night"`. Valid theme strings are the ones from `Ctrl+Y` in the
TUI (e.g. `doom`, `Catppuccin Mocha`, `Tokyo Night`, `Gruvbox Dark`, `Catppuccin
Latte`, …).

On success the script prints the screenshots dir and writes
`<dir>/manifest.json` — a JSON array of `{file, label, screen, keys}` objects. Read
it to learn what each PNG depicts.

If VHS fails or a screenshot is missing, report which states were captured and
continue the review with whatever PNGs exist — do not silently claim full coverage.

## Phase 2 — Analyze

1. Read `manifest.json`.
2. **`Read` each PNG in turn** (the Read tool renders the image so you can see the
   actual rendered terminal). Look carefully at layout, color, text, borders,
   highlighting, and spacing.
3. For each screen, produce concrete findings. Tag **every finding** with:
   - a **severity**: `blocker` (broken/unusable), `warning` (notable friction), or
     `nit` (polish);
   - a **lens** — one of the four below.

   The four review lenses (apply all of them):
   - **visual** — visual hierarchy, contrast/legibility, alignment, spacing, color
     usage, information density.
   - **usability** — discoverability of keybindings, affordances, navigation
     friction, empty/edge states.
   - **consistency** — cross-panel consistency of layout, borders, highlighting, and
     interaction patterns.
   - **accessibility** — color-only signalling (no text/shape backup), contrast
     ratios, legibility at small sizes.

   Each finding = a short title + 1–2 sentences of explanation + a concrete
   suggested fix. Be specific and grounded in what the screenshot actually shows;
   do not invent problems you cannot see.
4. **Cross-screen synthesis**: note inconsistencies between panels, navigation
   friction across the whole flow, and a short ordered list of top recommendations.

## Phase 3 — Report (native website docs page, default)

By default this skill produces a **native page in the project website** under
`website/docs/ui-review.html`, written as an **Eleventy content page** (front
matter + bare body) that inherits the shared nav, the full `_includes/sidebar.njk`
menu, and the CSS chrome from the `docs.njk` layout — so it never drifts from the
other docs pages. The screenshots are copied into `website/assets/ui-review/`, and
the review-specific widget styles live in `website/css/ui-review.css` (pulled in
via the page's `extraCss`). The page is already wired into the shared sidebar's
**"Developer"** section and a hub card on `website/docs/index.html`. This is what
publishes to GitHub Pages.

1. Write your Phase-2 findings to a JSON file (e.g. `target/ui-review/findings.json`)
   matching the schema documented at the top of `scripts/build_report.py`:

   ```json
   {
     "title": "thurbox TUI — UI/UX Review",
     "version": "<thurbox --version>", "theme": "Doom",
     "generated_at": "<date -u '+%Y-%m-%d %H:%M UTC'>",
     "recommendations": ["...top cross-screen recs..."],
     "screens": [
       {"file": "01-session-list.png", "label": "...", "keys": "launch",
        "findings": [{"severity": "warning", "lens": "visual",
                      "title": "...", "desc": "...", "fix": "..."}]}
     ]
   }
   ```

   `severity ∈ {blocker, warning, nit}`, `lens ∈ {visual, usability, consistency,
   accessibility}`. Severities/lenses map to the site's CSS color vars automatically.

2. Build the page:

   ```bash
   python3 "$SKILL_DIR/scripts/build_report.py" \
     --findings target/ui-review/findings.json \
     --shots <screenshots-dir> \
     --repo "$(git rev-parse --show-toplevel)"
   ```

   It copies the PNGs into `website/assets/ui-review/`, writes the Eleventy content
   page `website/docs/ui-review.html`, and prints the page path. The sidebar entry
   (`_includes/sidebar.njk` "Developer" section) and the `index.html` hub card are
   already in place — the generator never edits other pages, so no per-page wiring
   is needed.

3. Preview locally by building the site (`npm ci && npx @11ty/eleventy --serve`,
   then open the printed URL — a front-matter page won't render if opened directly),
   then **publish**: commit the new/updated `website/**` files on a branch, push, and
   open a PR. The `.github/workflows/pages.yml` workflow deploys `website/` to GitHub
   Pages on merge to `main`. Use the `/ship` skill (or `gh pr create`) to open the PR.

### Standalone self-contained variant (optional)

If the user explicitly wants a single portable file (e.g. to email) instead of a site
page, fall back to `templates/report.html.tmpl`: base64-encode each PNG
(`base64 -w0 <png>`) into `<img src="data:image/png;base64,…">`, fill `{{TITLE}}`,
`{{VERSION}}`, `{{THEME}}`, `{{GENERATED_AT}}`, `{{SUMMARY}}`, `{{CARDS}}` (severity
badge classes `.badge.blocker/.warning/.nit`, lens tags `.lens.<lens>`), and write to
`target/ui-review/report.html`. This file embeds everything and is not part of the
site.

Keep the findings honest and actionable. The goal is a report a maintainer can skim
in two minutes and act on.
