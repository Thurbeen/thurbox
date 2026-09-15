-- A pane that reads every field the kernel publishes on `thurbox.granted`,
-- `.platform`, `.metrics`, `.hover`, `.preflight.mux`, `.settings`, `.theme.roles`,
-- the four creation-flow reads and `.runs` — the injected tables no bundled pane
-- reads in a form selene can see — each as a plain dotted path.
--
-- It ends with the stdlib names in the same position: `_VERSION`, and the five
-- `math` functions Lua 5.4 has that `thurbox.yml` did not list. They are not
-- published by anything, but they are reachable in the VM and were rejected for
-- the same reason the tables above were — nothing in `ui/` happens to name them
-- — so they would go unnoticed the same way.
--
-- Expected to lint CLEAN, and that is the whole assertion here; `typos/` holds
-- the other direction, one pane per table. selene checks a dotted path against
-- `thurbox.yml` one segment at a time, so a table declared as a bare property
-- rejects the exact expression `ui/README.md` and `docs/PLUGINS.md` tell an
-- author to write. Each of these was, and CI stayed green because nothing in
-- `ui/` or `examples/` reads them in a form selene can see.
return {
  name = "std_reads",
  render = function()
    local read = {
      tostring(thurbox.granted.run),
      tostring(thurbox.granted.program),
      thurbox.platform.os,
      thurbox.platform.arch,
      tostring(thurbox.metrics.system.cpu_percent),
      tostring(thurbox.metrics.system.memory_used),
      tostring(thurbox.metrics.system.memory_total),
      tostring(thurbox.metrics.sessions),
      tostring(thurbox.hover.id),
      tostring(thurbox.hover.role),
      thurbox.preflight.mux.binary,
      thurbox.preflight.mux.presence,
      thurbox.preflight.mux.advice,
      tostring(thurbox.settings.features.tasks),
      tostring(thurbox.settings.features.auto_update),
      tostring(thurbox.settings.two_panel_min_cols),
      tostring(thurbox.settings.three_panel_min_cols),
      tostring(thurbox.settings.scrollback_lines),
      tostring(thurbox.theme.roles.accent),
      tostring(thurbox.theme.roles.status_working),
      tostring(thurbox.theme.roles.diff_removed_bg),
      tostring(thurbox.bookmarks.host),
      tostring(thurbox.bookmarks.loading),
      tostring(thurbox.bookmarks.rows),
      tostring(thurbox.browse.dir),
      tostring(thurbox.browse.error),
      tostring(thurbox.browse.entries),
      tostring(thurbox.branches.repo),
      tostring(thurbox.branches.list),
      tostring(thurbox.worktrees.repo),
      tostring(thurbox.worktrees.list),
      -- Keyed by the key this plugin passed to `run`, so a literal is a dotted
      -- path where every other map is only ever reached through a variable.
      tostring(thurbox.runs.cpu),
      tostring(thurbox.runs.cpu.state),
      _VERSION,
      tostring(math.deg(math.atan(1, 1)) + math.rad(90) + math.acos(0) + math.asin(1)),
    }
    return { text = table.concat(read, " ") }
  end,
}
