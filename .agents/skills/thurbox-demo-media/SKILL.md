---
name: thurbox-demo-media
description: Generating thurbox's demo media: the hero demo recorder (scripts/demo/record-hero.sh, asciinema + a stand-in ssh for the remote host), the VHS tapes driven by scripts/demo/record.sh (currently stale, recorded against v1), the asciinema-based doom easter-egg recorder, and the tutorial screenshot recorder whose stills and prose must change together. Use when re-recording or editing demo GIFs/MP4s, the website media, or the tutorial screenshots.
---

# Thurbox demo videos and screenshots

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Demo Video

**The hero demo is its own recorder.** `scripts/demo/record-hero.sh` writes
`media/thurbox-demo.{gif,mp4}` (the README GIF, the website's first video) and
`website/assets/thurbox-demo-poster.webp` in one run. It is not a tape: thurbox
runs under asciinema inside a private tmux server, `tmux send-keys` presses the
real chords, and the context menu is opened by sending the SGR mouse report of a
right press, which VHS cannot do. The remote host `devbox` is declared in the
sandbox's `hosts.toml` and reached through a **stand-in `ssh`** on PATH that runs
the command on this machine under devbox's own HOME and tmux socket directory —
the same stand-in `tests/backend_routes.rs` uses, so everything past the `ssh`
binary is the real remote path and nobody needs a second machine to rerun it.
Every socket it creates has a name only it uses (`thurbox-hero*`), never
`thurbox`, because its teardown kills them by name. agg needs a Nerd Font and
Noto Sans Symbols 2 in `FONT_DIR` (the header says why only the first is named).
The README GIF is a second agg pass at a smaller font, not an ffmpeg re-encode,
which turned agg's change-only frames into ~66 MB of full frames.

> **The VHS recordings are stale.** Every clip a tape produces was recorded against
> v1 and shows panes the interface no longer has — code review, the file viewer, the
> tasks and automations panels. The tapes themselves drive v1 chords, so they need
> rewriting before re-recording is worth doing. Until then the website's feature
> clips and the README's session-creation and interface GIFs show an interface
> that is gone.
> `doom-easter-egg.mp4` is the exception: it is not a tape, and it was re-recorded
> against v2 when Doom became a plugin (below).

The media is **generated**, not hand-recorded. One script drives the *real* TUI via
[VHS](https://github.com/charmbracelet/vhs) (needs `vhs` + `ffmpeg` + `ttyd` +
`tmux`) and writes GIF **and** MP4 into `media/`:

```bash
scripts/demo/record.sh                 # regenerate ALL demo videos
scripts/demo/record.sh theme search    # re-record a subset
```

One VHS tape each (`scripts/demo/<feature>.tape`); no args = all, otherwise tape
stems. Every clip uses **real agent CLIs**, one session per installed CLI, with
`HOME` overridden so agents boot with fresh history (keyring-authenticated CLIs stay
logged in but show no account email).

**The scenario is a realistic one, not a UI tour.** The repo is a vendored snapshot
of thurbox's own tree (a fixed file list copied into the throwaway `HOME` and `git
init`ed there — MIT, already local, so recordings stay hermetic and offline).
Sessions are named after the *work*, not the agent (`fix-osc52-tmux`,
`add-wsl-host-tests`, `perf-session-order-cache`, `docs-remote-hooks`), so the list
reads as one backlog with four branches in flight. The seeded tasks/automation and
the queries typed in the tapes are keyed to that same narrative, so **editing one
means editing the others**.

It runs fully isolated: a dev build uses the `thurbox-dev` socket and XDG subdirs,
and the script points `TMUX_TMPDIR` + `XDG_{DATA,CONFIG,STATE,CACHE}_HOME` at a
throwaway temp dir. **`TMUX_TMPDIR` is essential** — the `thurbox-dev` socket *name*
is shared by every dev build, so without a private socket directory the cleanup
`kill-server` would tear down dev sessions you have running.

`.github/workflows/pages.yml` copies the mp4s into `website/assets/` at deploy time
and `README.md` embeds the gifs, so regenerating them propagates everywhere. A
clip whose dimensions change also changes the `width`/`height` on its `<video>`
in `website/`, or the page reflows when it decodes.

**The website's `iddqd` easter egg is a separate recording.**
`scripts/demo/record-doom.sh` writes `media/doom-easter-egg.mp4` plus its
poster, and it is not a VHS tape: asciinema records the real TUI and agg rasterises
the cast. Doom is a **plugin** there, not an agent — the script installs
[`thurbox-doom`](https://github.com/Thurbeen/thurbox-doom) into the throwaway
interface directory, seeds the `program` grant the settings modal would have written
(trust is a decision made *in* a running interface, so there is deliberately no CLI
for it), and presses `f7`. Everything the clip keeps is timed from that press, so
read the phase comments before changing any of the numbers — including which frame
becomes the poster, since Doom flashes the whole view red when the player is hit.

**The onboarding tutorial's screenshots are a third recorder.**
`scripts/demo/record-tutorial.sh` writes `media/tutorial/*.png` — one still per
step of `docs/TUTORIAL.md`, taken by driving the real TUI through the creation flow.
Three things separate it from the tapes. The profile starts with **zero sessions and
an empty repo memory**, because the tutorial's subject is a first launch. It is not
VHS: thurbox runs in a detached tmux session, `tmux send-keys` presses the keys, and
each still is a `capture-pane -e` dump replayed through agg — which means the **real
chords** are pressed, where a tape has to rebind `Ctrl+/` and the F-keys VHS cannot
emit. And `HOME` is a short symlink into the sandbox (`/tmp/tutorial-home`) with the
XDG roots under it, because the repo picker and `session list` print absolute paths
and a `mktemp` name in them is a path no reader recognises as their own. Prose and
stills are one artifact: **a step that changes needs both re-recorded and rewritten**.
The walkthrough exists twice — `docs/TUTORIAL.md` for a checkout, and
`website/docs/tutorial.html` (screenshots copied to `website/assets/tutorial/`,
page styles in `website/css/tutorial.css`) for the site. They are separate documents
in different voices, not a generated pair, so **a step edited in one is edited in
both**; the recorder writes only `media/tutorial/`.

