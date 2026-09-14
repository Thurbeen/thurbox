-- A pane that reads every field the kernel publishes on `thurbox.granted`,
-- `.platform`, `.metrics`, `.hover` and `.preflight.mux` — the injected tables no
-- bundled pane reads in a form selene can see — each as a plain dotted path.
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
    }
    return { text = table.concat(read, " ") }
  end,
}
