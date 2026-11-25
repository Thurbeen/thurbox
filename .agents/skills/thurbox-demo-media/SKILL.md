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
Noto Sans Symbols 2 in `FONT_DIR`: the Nerd Font is JetBrains Mono Nerd Font
Mono, and Noto carries the worktree mark `⑂` (U+2442) no Nerd Font has. Name
only the first in `FONT_FAMILY`: agg falls back to the rest of `FONT_DIR` on its
own, and agg 1.x draws no text at all when the Noto family is named too.
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
poster: asciinema records the real TUI and agg rasterises the cast. Doom is a **mod** there, not an agent — the script installs
[`thurbox-doom`](https://github.com/Thurbeen/thurbox-doom) into the throwaway
interface directory, seeds the `program` grant the settings modal would have written
(trust is a decision made *in* a running interface, so there is deliberately no CLI
for it), and presses `f5`. Everything the clip keeps is timed from that press:
`trim-cast.mjs` prints when the first Doom frame appears, which is the origin to
retime the phase variables from. Check which frame becomes the poster after any
change, since Doom flashes the whole view red when the player is hit.
`DOOM_PLUGIN_SRC` points the install at a local checkout of the mod instead of
the GitHub clone, and `FONT_DIR`/`FONT_FAMILY` choose agg's fonts.

**The herdr guide's clip is a GUI recording.** `scripts/demo/record-herdr.sh`
writes `media/herdr-fleet-vega.{mp4,gif}`, the clip at the top of
`docs/runtime/herdr-users.md`. It records herdr's desktop app on a private Xvfb
display with ffmpeg's `x11grab` while a scratch Fleet runs VEGA and the worker
Tasks it applies, on three throwaway repositories (`backend`, `frontend`,
`docs`), with the logged-in `claude`: the agents do real work and spend real
quota. The request is typed into VEGA's own session a key at a time with
`hc keys`, which takes letters (capitals as `shift+<letter>`), digits, space
and `, . / ' -`, not `: { } " ?`, so the request avoids those. It keeps apart from the machine's own
Fleet and herdr through a herdr-core session of its own (`HC_SESSION`), a
scratch `FLEET_CONFIG` and store, a `HERDR_APP_HOME` with the welcome marked
seen, and the key prefix `herdrdemo`, whose worktrees and start records it
removes on exit. It does not move `XDG_DATA_HOME`: Claude Code installs its
updates there and repoints `~/.local/bin/claude` at them, so a scratch data
directory left the launcher dangling once removed; the run sets
`DISABLE_AUTOUPDATER=1` as well. It answers Claude's folder-trust dialog (the
default choice is "No, exit") and nothing else: a worker whose brief stays
unsent in its composer (`[Pasted text`) for 90 s fails the take rather than
being nudged. The stretch while the agents
work is sped up to `WORK_SECONDS`; the typing stays real time. Run a copy of
the script, not the file itself, while editing it: bash reads a script as it
runs.

**The onboarding tutorial's screenshots are a fourth recorder.**
`scripts/demo/record-tutorial.sh` writes `media/tutorial/*.png` — one still per
step of `docs/thurbox/tutorial.md`, taken by driving the real TUI through the
creation flow. The profile starts with **zero sessions, an empty repo
memory and no Fleet config**, because the tutorial's subject is a first launch:
the recorder runs `fleet init` and starts the sandbox's `fleet up` first, the
steps the tutorial asks of a reader, since thurbox never sets up Fleet.
`TEXT_DIR=<dir>` also keeps each frame as plain text, to check a re-record
against the prose without opening the PNGs. thurbox runs in a
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

