---
name: thurbox-demo-media
description: Generating thurbox's demo media: the hero demo recorder (scripts/demo/record-hero.sh, asciinema), the asciinema-based doom easter-egg recorder, and the tutorial screenshot recorder whose stills and prose must change together. Use when re-recording or editing demo GIFs/MP4s, the website media, or the tutorial screenshots.
---

# Thurbox demo videos and screenshots

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Demo Video

**The hero demo is its own recorder.** `scripts/demo/record-hero.sh` writes
`media/thurbox-demo.{gif,mp4}` (the README GIF, the website's first video) and
`website/assets/thurbox-demo-poster.webp` in one run. thurbox runs under
asciinema inside a private tmux server, `tmux send-keys` presses the real chords,
and the context menu is opened by sending the SGR mouse report of a right press.
Every socket it creates has a name only it uses (`thurbox-hero*`), never
`thurbox`, because its teardown kills them by name. agg needs a Nerd Font and
Noto Sans Symbols 2 in `FONT_DIR` (the header says why only the first is named).
The README GIF is a second agg pass at a smaller font, not an ffmpeg re-encode,
which turned agg's change-only frames into ~66 MB of full frames.

**Recordings are not committed.** `media/*.gif` and `media/*.mp4` are
gitignored. After recording, run `scripts/demo/publish-media.sh [FILE...]`. It
replaces the assets on the `media` GitHub release (a prerelease, so the update
check ignores it) and dispatches `pages.yml`, which downloads the MP4s into
`website/assets/`. The README links the hero GIF at
`https://github.com/Thurbeen/thurbox/releases/download/media/<file>`. A
replaced asset has no history: keep a copy if you may want the old take back.
`cd.yml` finds the last release with `git describe --match 'v*'` so the
`media` tag never stands in for a version.

A fresh checkout has no recordings. For a local website preview
(`npm run dev:website`), fetch the videos the deploy uses:

```bash
gh release download media --pattern '*.mp4' --dir website/assets --clobber
```

`.github/workflows/pages.yml` copies the mp4s into `website/assets/` at deploy time
and `README.md` embeds the hero GIF, so regenerating them propagates everywhere. A
clip whose dimensions change also changes the `width`/`height` on its `<video>`
in `website/`, or the page reflows when it decodes.

**The website's `iddqd` easter egg is a separate recording.**
`scripts/demo/record-doom.sh` writes `media/doom-easter-egg.mp4` plus its
poster: asciinema records the real TUI and agg rasterises the cast. Doom is a **plugin** there, not an agent — the script installs
[`thurbox-doom`](https://github.com/Thurbeen/thurbox-doom) into the throwaway
interface directory, seeds the `program` grant the settings modal would have written
(trust is a decision made *in* a running interface, so there is deliberately no CLI
for it), and presses `f7`. Everything the clip keeps is timed from that press, so
read the phase comments before changing any of the numbers — including which frame
becomes the poster, since Doom flashes the whole view red when the player is hit.

**The onboarding tutorial's screenshots are a third recorder.**
`scripts/demo/record-tutorial.sh` writes `media/tutorial/*.png` — one still per
step of `docs/thurbox/tutorial.md`, taken by driving the real TUI through the
creation flow. The profile starts with **zero sessions and an empty repo
memory**, because the tutorial's subject is a first launch. thurbox runs in a
detached tmux session, `tmux send-keys` presses the real chords, and each still
is a `capture-pane -e` dump replayed through agg. `HOME` is a short symlink into
the sandbox (`/tmp/tutorial-home`) with the XDG roots under it, because the repo
picker prints absolute paths and a `mktemp` name in them is a path no reader
recognises as their own. Prose and stills are one artifact: **a step that
changes needs both re-recorded and rewritten**.
The walkthrough exists twice — `docs/thurbox/tutorial.md` for a checkout, and
`website/docs/tutorial.html` (screenshots copied to `website/assets/tutorial/`,
page styles in `website/css/tutorial.css`) for the site. They are separate documents
in different voices, not a generated pair, so **a step edited in one is edited in
both**; the recorder writes only `media/tutorial/`.

