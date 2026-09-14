-- The same reads, one letter wrong on each table.
--
-- The other direction from `reads.lua`: declaring the fields must not become a
-- wildcard that accepts whatever is asked for, because catching a misspelt
-- field is the only reason this standard library describes the injected tables
-- at all — `thurbox.sesions` renders an empty pane and says nothing.
return {
  name = "std_typos",
  render = function()
    local read = {
      tostring(thurbox.granted.prgoram),
      thurbox.platform.arhc,
      tostring(thurbox.metrics.sesions),
      tostring(thurbox.metrics.system.cpu_precent),
      tostring(thurbox.hover.roel),
      thurbox.preflight.mux.binray,
    }
    return { text = table.concat(read, " ") }
  end,
}
