local theme = require("lib.theme")

return {
  name = "theme_probe",
  render = function()
    return { text = "warning", style = { fg = theme.warning } }
  end,
}
