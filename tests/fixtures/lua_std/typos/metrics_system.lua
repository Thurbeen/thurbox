-- `thurbox.metrics.system`, with one letter wrong.
--
-- A level below `metrics`, because declaring a record only as deep as its own
-- name relocates the bug rather than fixing it.
return {
  name = "std_typo_metrics_system",
  render = function()
    return { text = tostring(thurbox.metrics.system.cpu_precent) }
  end,
}
