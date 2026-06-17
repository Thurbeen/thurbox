# Design: Automatic Worktree Cleanup

Status: **Proposed** (design only — implementation is a follow-up).
Tracking: thurbox task #54. Companion RFC issue solicits community input on the
policy knobs (opt-in vs default-ON, TTL, branch deletion).

This document is a design plan: it states the problem, surveys prior art,
presents the cleanup model with its options and trade-offs, and ends with a
concrete recommendation and a phased rollout. It does **not** change behavior on
its own.

---

## 1. Problem

Thurbox creates one git worktree per worktree-backed session under
`~/.local/share/thurbox/worktrees/<repo-hash>/<sanitized-branch>`
(`git::create_worktree_on`, `git::worktree_path`). It has **no automatic
cleanup path**:

- When a session is deleted in the TUI, its worktree is **intentionally left on
  disk** so `Ctrl+U` can restore it. See `App::finalize_pending_delete`
  (`src/app/mod.rs`): _"Worktrees are intentionally left on disk for Ctrl+U
  restore."_ The DB row is soft-deleted; the worktree is not.
- The **only** code that removes a worktree is `session delete --force`
  (headless, `session_ops::delete::force_teardown`) and the rollback path when
  worktree creation half-fails (`app::create_worktrees`).
- `git worktree prune` is **never** run, so git's per-repo worktree
  administrative metadata (`.git/worktrees/<id>`) accumulates even when the
  checkout directory is gone.

### Observed impact

On a real long-running install, **~90% of the worktree directories on disk had
no live session** — orphans left behind by deleted sessions, accumulating into a
large amount of wasted disk. Three distinct leak sources compound:

1. **Soft-deleted sessions** — worktree tracked in the `worktrees` table but the
   session row is soft-deleted; kept forever for a restore that rarely comes.
2. **Filesystem orphans** — worktree directories with **no DB row at all**
   (e.g. created before a schema change, force-killed TUI, manual `git worktree
   add` under the thurbox dir, or a crash between worktree creation and DB
   write).
3. **Stale git metadata** — `.git/worktrees/<id>` entries whose checkout is gone
   but `git worktree prune` was never run; and fully-merged branches that linger.

A complete solution must reconcile all three against the set of **live
sessions**, not just tidy the first.

---

## 2. Goals / non-goals

**Goals**

- Reclaim disk from worktrees that no live session needs, safely and
  predictably.
- Preserve the existing `Ctrl+U` restore affordance via a **retention window**.
- Never destroy uncommitted or unpushed work without an explicit, informed
  override.
- Work both with the TUI open and fully headless (the heartbeat keeps firing).
- Be agent-neutral and config-driven, consistent with thurbox's existing knobs.

**Non-goals**

- Touching the user's actual repositories or any non-thurbox worktrees. Cleanup
  only ever operates within thurbox's `worktrees_dir`.
- Rewriting history, pushing, or any network mutation (cleanup may *read*
  ahead/behind state, never push).
- Forcing a policy: irreversible auto-deletion stays opt-in (see §6).

---

## 3. Prior art

| Tool | Trigger | Classification | Safety | TTL |
|------|---------|----------------|--------|-----|
| [PolyPilot #201](https://github.com/PureWeen/PolyPilot/issues/201) | app startup / new-session | 3 tiers: safe-auto (clean + remote-gone), prompt (uncommitted), keep (active session) | runs `git worktree prune` after | none — session-correlation only |
| [claude-worktree-tools `wt-cleanup`](https://github.com/ThinkVelta/claude-worktree-tools/blob/main/templates/skills/wt-cleanup/SKILL.md) | manual skill | stale = last commit > 7d **and** clean | `git status --porcelain` dirty guard; `git branch --merged` detection; safe `git branch -d` (never `-D`) | 7-day inactivity |
| git native | `git gc` | n/a | refuses to remove a worktree with a dirty/locked tree without `--force` | `gc.pruneworktreesexpire` default **3 months** |
| [lazyworktree](https://github.com/chmouel/lazyworktree) / Git Worktree Tidy | manual TUI/CLI | "show what could be cleaned, let user decide" | per-worktree teardown hooks; confirmation before destructive ops | inactivity-based |

**Takeaways that shape this design**

- A **risk-tiered classification** (safe / prompt / keep) is the consensus
  model — adopt it.
- The conventional safe bar is **clean working tree + merged-or-gone branch +
  no active session**; everything else needs confirmation.
- Always finish with **`git worktree prune`** to clear orphaned metadata.
- Prefer **`git branch -d`** (safe) over `-D` so git itself refuses to drop
  unmerged commits.
- Tools lean **conservative / opt-in** for irreversible deletes; time-based TTLs
  range from 7 days to git's 3 months.

---

## 4. The model: two layers

### Layer 1 — Soft delete (recoverable)

Deleting a session keeps the existing behavior — the worktree stays on disk and
`Ctrl+U` restores it — but is now **stamped and marked stale**:

- The session's `worktrees` rows are soft-deleted (already happens:
  `soft_delete_worktrees`), and the soft-delete carries a `deleted_at`
  timestamp (the column already exists; we begin *reading* it for eligibility).
- The worktree is now a *candidate* for hard-delete once it ages past the
  retention window (§6).

No data is lost in Layer 1; it only changes the worktree's **status**, which
Layer 2 acts on.

### Layer 2 — Hard delete (irreversible)

Fully removes the worktree and its git footprint:

1. `git worktree remove <path>` (force only when classification permits — see
   §5).
2. `git worktree prune` in the parent repo to clear stale `.git/worktrees/<id>`
   metadata (also reaps filesystem-orphan metadata).
3. Optionally `git branch -d <branch>` (safe delete) when the branch is fully
   merged and `prune_merged_branches` is enabled.
4. Remove the now-empty `<repo-hash>/` parent dir if it holds nothing else.
5. Hard-delete the DB rows (`hard_delete_worktrees`) so the record matches disk.

Layer 2 runs **manually on demand always**, and **automatically only when
opt-in auto-purge is enabled** (§6).

---

## 5. Classification

Every worktree thurbox knows about (DB rows) **plus** every directory found
under `worktrees_dir` (filesystem scan) is classified into one bucket. Live
sessions are the anchor: a worktree "belongs to a live session" iff a
non-deleted session row references its path.

| Tier | Conditions | Auto-purge? | Manual `clean`? |
|------|-----------|-------------|-----------------|
| **KEEP** | Referenced by a live (non-deleted) session, **or** has a running agent process | never | never |
| **SAFE** | No live session **and** clean working tree (`git status --porcelain` empty) **and** branch merged into default **or** fully pushed (ahead == 0) **and** age past TTL | yes (when enabled) | yes |
| **NEEDS-FORCE** | No live session but **uncommitted changes** or **unpushed commits** (ahead > 0 and unmerged) | never (surfaced, skipped) | only with `--force` |
| **ORPHAN-META** | `.git/worktrees/<id>` metadata whose checkout dir is gone | yes (`prune`) | yes |
| **ORPHAN-DIR** | Directory under `worktrees_dir` with no DB row and not a valid git worktree (e.g. leftover empty dir) | behind `--include-untracked` | with `--include-untracked` |

Notes:

- **Dirty / unpushed is the hard line.** Auto-purge *never* touches NEEDS-FORCE.
  It is listed in reports so the user can act, but removing it requires an
  explicit `--force` on the manual command. This is the single most important
  safety property: **automatic cleanup cannot cause data loss.**
- **"Merged or pushed"** uses the existing `git::ahead_behind` and a
  `git branch --merged <default>` check (default branch via the existing
  `git::default_branch` / `default_branch_from_remote`). Either condition makes
  the commits recoverable, so the worktree is SAFE.
- Classification is **pure and testable** given a snapshot of (sessions, disk
  scan, per-worktree git status); the side-effecting sweep consumes the
  classification.

---

## 6. Configuration

A new `[worktree]` section in `settings.toml` (seeded commented-out, defaults
apply when absent — matching every other knob):

```toml
[worktree]
# Automatically hard-delete SAFE worktrees once they age past retention.
# OFF by default: hard-delete is irreversible, so it is opt-in.
auto_purge = false

# Retention window (days) after a session is deleted before its clean,
# sessionless worktree becomes eligible for auto-purge.
retention_days = 7

# When auto-purging, also `git branch -d` the worktree's branch if it is
# fully merged into the default branch. Safe delete — git refuses unmerged.
prune_merged_branches = false

# Also remove untracked, non-git directories found under the worktrees dir
# (filesystem orphans with no DB row). Conservative: off by default.
include_untracked = false
```

**Why these defaults**

- `auto_purge = false` — **opt-in**. Hard-delete is irreversible; the safe
  manual command (§7) covers the common "I want my disk back" case without ever
  surprising a user by deleting a worktree. This matches the conservative posture
  every comparable tool takes. *(Open question in the RFC: default-ON instead.)*
- `retention_days = 7` — long enough that `Ctrl+U` restore stays useful for a
  recently deleted session, short enough to actually reclaim disk. Aligns with
  the most common prior-art TTL. *(Open question: 7 vs 14 vs 30.)*
- `prune_merged_branches = false` — deleting branches is a separate, surprising
  side effect; keep it explicit.
- `include_untracked = false` — never delete a non-git directory automatically.

The manual command **always works** regardless of `auto_purge`.

---

## 7. Surfaces (triggers + UX)

### 7.1 Manual CLI command — always available

New `thurbox-cli worktree` subcommand:

```
thurbox-cli worktree list                 # classify + show every worktree, tiered
thurbox-cli worktree clean [--dry-run]    # hard-delete SAFE + ORPHAN-* tiers
                          [--force]        # also remove NEEDS-FORCE (dirty/unpushed)
                          [--ttl-days N]   # override retention for this run
                          [--include-untracked]
                          [--repo <path>]  # scope to one repo
                          [--json|--pretty|--text]
```

- `clean` defaults to acting on SAFE + ORPHAN tiers, **prints a summary first**,
  and honors `--dry-run` to preview. NEEDS-FORCE is listed but skipped unless
  `--force`. KEEP is never touched.
- Output follows the existing CLI convention (human by default, JSON when piped;
  every mutating command carries a `summary` line and per-item results).

### 7.2 Automation heartbeat `tick` — headless auto-purge

When `auto_purge = true`, the headless `automation tick`
(`cli::automations`) runs a cleanup sweep at the top of the tick, right beside
the existing message/audit pruning (`prune_old_messages`,
`prune_messages`). This fires even with the TUI closed, via the existing tmux
heartbeat keeper — the same mechanism flow/forge/shepherd rely on. Gated on
`[features] automations = true` (no heartbeat → no headless sweep), mirroring
how headless extension self-heal is gated.

### 7.3 TUI startup sweep + affordance

- **Startup reconcile**: on TUI launch (`main.rs`, alongside extension
  self-heal), when `auto_purge = true`, run one SAFE-tier sweep so a
  long-closed TUI catches up. Always run the cheap `git worktree prune` for
  ORPHAN-META regardless of `auto_purge` (pruning dead metadata is non-
  destructive).
- **Surfacing (no auto-purge)**: even with `auto_purge = false`, the TUI can
  surface "N stale worktrees · ~X GB reclaimable — run `worktree clean`" as a
  startup status toast, turning the silent leak into a visible, actionable
  nudge. (Conservative: surface, don't act.)

A full TUI management pane is out of scope for v1; the status nudge + CLI cover
the need. A later iteration could add an interactive picker.

---

## 8. Storage

The `worktrees` table already has the columns we need:

- `created_at`, `deleted_at` (soft-delete timestamp) — eligibility is
  `now - deleted_at >= retention_days` for soft-deleted sessions.
- `repo_path`, `worktree_path`, `branch` — enough to locate and classify.

Likely **no schema migration** is required for the core flow. New read queries
in `src/storage/worktrees.rs`:

- `list_all_worktrees()` / `list_deleted_worktrees(before: ts)` — across
  sessions, for the sweep (current `get_worktrees` is per-session only).
- A query joining against live sessions to compute the KEEP set.

The **filesystem scan** (ORPHAN-DIR / ORPHAN-META) reads `worktrees_dir`
directly and diffs against the DB + `git worktree list --porcelain`; no new
table. Audit each hard-delete under the existing `EntityType::Worktree` audit
log (the table is already audited on create).

---

## 9. Code touch points (for the follow-up implementation)

| Area | Change |
|------|--------|
| `src/git/mod.rs` | `prune_worktrees(repo)`, `worktree_is_merged(repo, branch)`, `delete_branch_if_merged`, `list_git_worktrees(repo)` (porcelain parse) |
| `src/storage/worktrees.rs` | `list_all_worktrees`, `list_deleted_worktrees(before)`, live-session join query |
| `src/session_ops/worktree_clean.rs` *(new)* | pure classification (`classify_worktrees`) + side-effecting sweep (`run_cleanup`) reused by CLI, tick, and startup |
| `src/cli/*` | new `worktree` subcommand (`list` / `clean`) wired into dispatch |
| `src/cli/automations.rs` | call the sweep at the top of `tick` when `auto_purge` |
| `src/app/mod.rs` / `src/main.rs` | startup `git worktree prune` always; SAFE sweep + status nudge gated on config |
| `src/session/` config | parse `[worktree]` settings (mirrors existing settings loader) |
| `docs/CONFIG.md`, `docs/FEATURES.md` | document the `[worktree]` knobs + behavior (per the repo doc rule) |
| Tests | `classify_worktrees` table tests (each tier), retention boundary, dirty/unpushed guard, dry-run no-op, orphan-dir detection |

The classify/sweep split keeps the destructive logic in one tested place
(`session_ops`), reachable from the CLI, the tick, and the TUI — matching the
existing `session_ops`/`cli` headless layering (architecture rules allow
`session_ops` and `cli` to reach `crate::agent::…`/`crate::git::…` via
fully-qualified paths).

---

## 10. Safety summary

1. **Auto-purge is opt-in** (`auto_purge = false` by default).
2. **Auto-purge only ever removes the SAFE tier** — clean, merged-or-pushed,
   sessionless, past TTL. Dirty/unpushed → NEEDS-FORCE → never auto-deleted.
3. **Live sessions are never touched** (KEEP), checked against the DB + running
   process, not just the worktree path.
4. **Retention window** preserves `Ctrl+U` restore for `retention_days`.
5. **Dry-run** previews; the manual command reports before acting.
6. **`git branch -d`, never `-D`** — git refuses to drop unmerged branches.
7. **Scope-limited** to `worktrees_dir`; real repos are never in range.
8. `git worktree prune` (non-destructive metadata cleanup) is the only thing
   that ever runs unconditionally.

---

## 11. Recommendation

Ship in two phases:

**Phase 1 (safe, high value, default-on behavior)**

- Classification + `git worktree prune`.
- `thurbox-cli worktree list` / `clean` (manual, with `--dry-run` / `--force`).
- Soft-delete `deleted_at` eligibility plumbing.
- TUI startup status nudge ("N stale worktrees, ~X GB — run `worktree clean`")
  and unconditional metadata prune.

This alone reclaims the bulk of the leaked disk with zero risk of data loss and
no irreversible automation.

**Phase 2 (opt-in automation)**

- `[worktree] auto_purge` honored in the heartbeat `tick` and TUI startup.
- `prune_merged_branches`, `include_untracked` knobs.

**Default posture: opt-in (`auto_purge = false`).** Hard-delete is irreversible,
the manual command covers the common case, and every comparable tool defaults
conservative. The RFC issue exists to test this call with the community — if the
consensus is that a 7-day clean-orphan auto-purge is safe enough to default on,
flipping `auto_purge`'s default is a one-line change.

---

## 12. Open questions (for the RFC / maintainer)

- **Opt-in vs default-ON** auto-purge. (Recommendation: opt-in.)
- **TTL default**: 7 vs 14 vs 30 days. (Recommendation: 7.)
- **Auto-delete merged branches?** (Recommendation: off by default.)
- Should the startup nudge appear even when `tasks`/`automations` features are
  off? (Recommendation: yes — it's a disk-hygiene concern, not a feature pane.)
