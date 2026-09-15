-- A pane that reads EVERY field `thurbox.yml` declares on the injected tables no
-- bundled pane reads in a form selene can see — `thurbox.granted`, `.platform`,
-- `.metrics`, `.hover`, `.preflight.mux`, `.settings`, `.theme.roles`, the four
-- creation-flow reads and `.runs` — each as a plain dotted path.
--
-- Every field, not a sample: a declaration this file does not name is one that
-- can be deleted from `thurbox.yml` with nothing failing. The counts in the
-- section comments are the whole declared set for that table, so a field added
-- there and not here is visible as a count that no longer matches.
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
      -- granted (2)
      tostring(thurbox.granted.run),
      tostring(thurbox.granted.program),
      -- platform (2)
      tostring(thurbox.platform.os),
      tostring(thurbox.platform.arch),
      -- metrics.system (3)
      tostring(thurbox.metrics.system.cpu_percent),
      tostring(thurbox.metrics.system.memory_used),
      tostring(thurbox.metrics.system.memory_total),
      -- hover (2)
      tostring(thurbox.hover.id),
      tostring(thurbox.hover.role),
      -- preflight.mux (3)
      tostring(thurbox.preflight.mux.binary),
      tostring(thurbox.preflight.mux.presence),
      tostring(thurbox.preflight.mux.advice),
      -- settings.features (13)
      tostring(thurbox.settings.features.tasks),
      tostring(thurbox.settings.features.automations),
      tostring(thurbox.settings.features.file_viewer),
      tostring(thurbox.settings.features.global_search),
      tostring(thurbox.settings.features.info_panel),
      tostring(thurbox.settings.features.shell_pane),
      tostring(thurbox.settings.features.code_review),
      tostring(thurbox.settings.features.perf_hud),
      tostring(thurbox.settings.features.mouse),
      tostring(thurbox.settings.features.notifications),
      tostring(thurbox.settings.features.soft_delete),
      tostring(thurbox.settings.features.version_check),
      tostring(thurbox.settings.features.auto_update),
      -- bookmarks (3)
      tostring(thurbox.bookmarks.host),
      tostring(thurbox.bookmarks.loading),
      tostring(thurbox.bookmarks.rows),
      -- browse (5)
      tostring(thurbox.browse.host),
      tostring(thurbox.browse.dir),
      tostring(thurbox.browse.loading),
      tostring(thurbox.browse.error),
      tostring(thurbox.browse.entries),
      -- branches (5)
      tostring(thurbox.branches.host),
      tostring(thurbox.branches.repo),
      tostring(thurbox.branches.loading),
      tostring(thurbox.branches.error),
      tostring(thurbox.branches.list),
      -- worktrees (5)
      tostring(thurbox.worktrees.host),
      tostring(thurbox.worktrees.repo),
      tostring(thurbox.worktrees.loading),
      tostring(thurbox.worktrees.error),
      tostring(thurbox.worktrees.list),
      -- theme.roles (33)
      tostring(thurbox.theme.roles.accent),
      tostring(thurbox.theme.roles.accent_bright),
      tostring(thurbox.theme.roles.app_bg),
      tostring(thurbox.theme.roles.border_focused),
      tostring(thurbox.theme.roles.border_unfocused),
      tostring(thurbox.theme.roles.branch_name),
      tostring(thurbox.theme.roles.danger),
      tostring(thurbox.theme.roles.diff_added),
      tostring(thurbox.theme.roles.diff_added_bg),
      tostring(thurbox.theme.roles.diff_removed),
      tostring(thurbox.theme.roles.diff_removed_bg),
      tostring(thurbox.theme.roles.inverted_fg),
      tostring(thurbox.theme.roles.keybind_hint),
      tostring(thurbox.theme.roles.modal_bg),
      tostring(thurbox.theme.roles.modal_border),
      tostring(thurbox.theme.roles.modal_dim_bg),
      tostring(thurbox.theme.roles.role_name),
      tostring(thurbox.theme.roles.search_bar),
      tostring(thurbox.theme.roles.selection_bg),
      tostring(thurbox.theme.roles.selection_fg),
      tostring(thurbox.theme.roles.status_blocked),
      tostring(thurbox.theme.roles.status_done),
      tostring(thurbox.theme.roles.status_error),
      tostring(thurbox.theme.roles.status_idle),
      tostring(thurbox.theme.roles.status_running),
      tostring(thurbox.theme.roles.status_unknown),
      tostring(thurbox.theme.roles.status_unreachable),
      tostring(thurbox.theme.roles.status_working),
      tostring(thurbox.theme.roles.text_muted),
      tostring(thurbox.theme.roles.text_primary),
      tostring(thurbox.theme.roles.text_secondary),
      tostring(thurbox.theme.roles.tool_allowed),
      tostring(thurbox.theme.roles.tool_disallowed),
      -- settings scalars (3), and the one map left at the table
      tostring(thurbox.settings.two_panel_min_cols),
      tostring(thurbox.settings.three_panel_min_cols),
      tostring(thurbox.settings.scrollback_lines),
      tostring(thurbox.metrics.sessions),
      -- Keyed by the key this plugin passed to `run`, so a literal is a
      -- dotted path where every other map is reached through a variable.
      tostring(thurbox.runs.cpu),
      tostring(thurbox.runs.cpu.state),
      -- The stdlib names this change declares.
      _VERSION,
      tostring(math.acos(0)),
      tostring(math.asin(1)),
      tostring(math.atan(1, 1)),
      tostring(math.deg(1)),
      tostring(math.rad(90)),
    }
    return { text = table.concat(read, " ") }
  end,
}
