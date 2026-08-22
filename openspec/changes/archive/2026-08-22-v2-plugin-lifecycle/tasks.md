## 1. Delivery learns what the user removed

- [x] 1.1 Give the manifest a third state — a shipped file may be recorded as
      written (with its digest) or as removed — deserializing a legacy
      digest-only manifest unchanged
- [x] 1.2 Materialize on the D1 table: absent + unknown writes, absent + known
      tombstones, present + matching updates, present + diverged preserves
- [x] 1.3 Retire a file we wrote that the binary no longer carries: delete it
      when unmodified, preserve and report it when the user changed it
- [x] 1.4 Report per file which of those happened, in a shape the inventory can
      read rather than only a startup log line
- [x] 1.5 Replace `a_deleted_file_comes_back` with its opposite, and cover:
      deletion surviving an upgrade that changes the file, a discarded directory
      re-delivering everything, a user-written file untouched by any of it

## 2. Restoring what was removed or edited

- [x] 2.1 Write a single embedded file back to the plugin directory and clear its
      manifest record, without touching any other file
- [x] 2.2 Accept `restore` and `remove` for one file as commands, so the pane that
      offers them needs no filesystem capability
- [x] 2.3 Confirm before removing. There is no shared confirmation surface in the
      bundled set to reuse — `60_confirm.lua` went with the panes that were cut —
      so it is two presses in the pane itself, which is lighter than a modal for
      one keystroke and cancels by moving away
- [x] 2.4 Verify the reload is the existing watcher path — writing the file is the
      whole trigger, with no second reload mechanism

## 3. The interface can see itself

- [x] 3.1 Retain, at load, why each plugin is not running, instead of discarding
      it: the environment error and the file it names
- [x] 3.2 Distinguish placed-and-visible, placed-but-not-active, and slot-not-
      placed from the arrangement's resolved slots after a frame
- [x] 3.3 Publish one inventory entry per plugin — name, path, slot, source,
      state, error — including removed plugins, which have no loaded plugin
      behind them
- [x] 3.4 Publish the directory the interface was loaded from
- [x] 3.5 An empty plugin set loads cleanly rather than faulting, so removing the
      last plugin does not summon the recovery floor

## 4. The inventory pane

- [x] 4.1 A bundled plugin listing every entry with its source and state, grouped
      so removed and failed plugins are not lost among healthy ones
- [x] 4.2 Restore and remove from the list, showing the directory in use in the
      pane's own frame
- [x] 4.3 Declare its key and its slot like any other pane — but **no footer
      entry**: the action band is bounded by what its entries leave, so a fourth
      one costs the focus badge its space at 60 columns, and this is a view you
      visit when something is wrong rather than a standing affordance. Confirm
      removing the pane itself leaves nothing else broken

## 5. What was already true, now held by tests

- [x] 5.1 Adding a pane, a decorator and a `lib/` module each work from a file
      drop alone, and delivery leaves all three alone. A decorator did **not**:
      the loader demanded a `render` of it and its default slot made it compete
      with the pane it decorates, so both were fixed
- [x] 5.2 Replacing a bundled pane under a different filename yields exactly one
      of it, keys and slot included
- [x] 5.3 Editing the arrangement adds and removes a region, and a plugin whose
      slot is gone neither draws nor faults nor holds focus

## 6. Documentation

- [x] 6.1 `docs/PLUGINS.md`: the lifecycle — add, edit, replace, remove, restore
      — and what an upgrade does to each
- [x] 6.2 `docs/PLUGINS.md`: the escape hatches, in the "when something goes
      wrong" section — the manifest, the directory, the inventory pane
- [x] 6.3 `CLAUDE.md`'s v2 section: the bundled set gains a pane, and removal is
      no longer undone at startup
