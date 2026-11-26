return {
  name = "std_typo_metrics_system",
  render = function()
    return { text = tostring(thurbox.metrics.system.cpu_precent) }
  end,
}
