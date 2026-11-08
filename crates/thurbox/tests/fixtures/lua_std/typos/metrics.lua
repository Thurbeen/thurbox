-- `thurbox.metrics`, with one letter wrong.
return {
  name = "std_typo_metrics",
  render = function()
    return { text = tostring(thurbox.metrics.sesions) }
  end,
}
