-- `thurbox.settings.features`, with one letter wrong.
return {
  name = "std_typo_settings_features",
  render = function()
    return { text = tostring(thurbox.settings.features.taskz) }
  end,
}
