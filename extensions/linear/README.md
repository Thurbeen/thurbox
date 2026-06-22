# linear (thurbox extension)

> **Experimental.** Bidirectionally syncs **Linear issues** with the thurbox
> task list: your issues show up as tasks, and marking a task done moves the
> issue to a completed state.

A `linear-tick` **automation** runs a deterministic sync script
(`scripts/sync.sh`) every 15 minutes — **no agent, no LLM, no tokens**. thurbox's
scheduler runs it (TUI or headless heartbeat) and records the result in the
automation run history. The script only calls `thurbox-cli` and Linear's GraphQL
API (over `curl`). Linear has **no CLI**, so all calls hit
`https://api.linear.app/graphql`.

## Setup

### 1. Prerequisites

- `thurbox-cli` **≥ 0.141** on `PATH` (needs the `Exec` automation action +
  `task --source/--external-id/--external-url`; check `thurbox-cli version`).
- `curl` and `jq`.

### 2. Get a Linear API key

Linear has no CLI, so the sync talks to its GraphQL API directly with a
**Personal API key** (Linear → **Settings → Security & access → Personal API
keys → New key**).

Because the automation runs **headless** (the scheduler fires it without a
reliable inherited shell environment), the robust way to hand the key to the
sync is a `credentials.env` file in the install home — `sync.sh` sources it
before running. Create `~/linear/credentials.env`:

```sh
mkdir -p ~/linear
printf 'LINEAR_API_KEY=lin_api_xxom\n' > ~/linear/credentials.env
chmod 600 ~/linear/credentials.env
```

(Exporting `LINEAR_API_KEY` from your shell profile, e.g. `~/.zshrc`/`~/.bashrc`,
is an alternative — but it only reaches the sync if the scheduler inherits your
profile, so `credentials.env` is recommended.)

The key is sent **verbatim** in the `Authorization` header — Linear personal API
keys are **not** prefixed with `Bearer`. Also note your **team key** (the prefix
on issue ids, e.g. `ENG` in `ENG-7`).

### 3. Install the extension

```sh
thurbox-cli extension install linear
# or from a checkout:
thurbox-cli extension install ./extensions/linear
```

This lays down `~/linear/` (override with `--home`) and activates the
`linear-tick` automation, which thurbox self-heals if deleted.

### 4. Configure the teams to sync

Edit `~/linear/trackers.md` — one row per Linear team, where `query` is the
**team key** (e.g. `ENG`):

```markdown
| name    | query | push_back |
|---------|-------|-----------|
| backend | ENG   | no        |
| web     | WEB   | no        |
```

Keep `push_back = no` for the first run (pull only — nothing is changed on
Linear).

### 5. First sync & verify

The automation fires every 15 min. To run it now, trigger it from the
**Automations** pane (`Ctrl+P` → select `linear-tick` → `r`) or headless:

```sh
thurbox-cli automation run <id>     # id from: thurbox-cli automation list
# or run the script directly:
~/linear/scripts/sync.sh
```

Then check the imported tasks:

```sh
thurbox-cli task list --json | jq -c '.[] | select(.source=="linear") | {id,status,external_id,title}'
```

Linear states map faithfully to task status (see the reference below).
Re-running is **idempotent** — dedup by `(source, external_id)` means unchanged
issues are left alone. The run's output (created/updated/unchanged counts) shows
in the automation run history.

### 6. Enable two-way sync (optional)

Set `push_back = yes` on a tracker row. Now marking an imported task **done**
moves its Linear issue to a completed state, and moving it back to **todo**
returns it to an unstarted (open) state (only the open↔done axis is pushed;
`in_progress` stays open). The sync runs **push-then-pull** so your local change
reaches Linear before the pull reads state back.

## trackers.md reference

- **query** — a Linear **team key** (e.g. `ENG`). The whole cell is the team
  key; all of that team's issues are synced.
- **push_back** — `yes` enables thurbox → Linear status push for that team.

Imported tasks carry `source=linear` and `external_id="<issue identifier>"`
(e.g. `ENG-7`), so re-syncing never duplicates them.

State mapping (Linear state type → thurbox task status):

| Linear state type                  | thurbox status |
|------------------------------------|----------------|
| `unstarted` / `backlog` / `triage` | `todo`         |
| `started`                          | `in_progress`  |
| `completed` / `canceled`           | `done`         |

## How it works

`scripts/sync.sh` (run by the automation) sources `~/linear/credentials.env` (so
`LINEAR_API_KEY` is available headless), then does, in order:

| Step | Script | Behavior |
|------|--------|----------|
| Push | `push-status.sh` | `done` → completed state, `todo`/`in_progress` → unstarted state (only `push_back=yes` rows) |
| Pull | `fetch.sh` + `upsert.sh` | List a team's issues → create/update tasks; dedup by `(source, external_id)` |

The whole thing is a **deterministic automation** — no agent, no LLM, no tokens.
Linear is authoritative for an issue's title, URL, and state; your local
`todo`↔`in_progress` distinction is preserved.

## Troubleshooting

- **Nothing syncs** — check the automation run history (`Ctrl+P`, or
  `thurbox-cli automation runs <id>`) for the script's output; run
  `~/linear/scripts/sync.sh` by hand to see errors directly.
- **`LINEAR_API_KEY is unset`** — create `~/linear/credentials.env` with
  `LINEAR_API_KEY=lin_api_xxom` (step 2). The headless automation can't see a
  variable you only exported interactively.
- **Empty pull / GraphQL error** — confirm the key is valid and the team key in
  `trackers.md` matches the issue id prefix (`fetch.sh "ENG"` should list
  issues, e.g. `ENG-7`).
- **`unknown option '--source'`** — your `thurbox-cli` predates 0.141; rebuild /
  update thurbox.

## Turn it off

```sh
thurbox-cli extension deactivate linear          # stop syncing
thurbox-cli extension uninstall linear --purge   # remove home + automation
```

Imported tasks remain in your task list (they are not deleted on uninstall).
