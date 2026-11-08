-- A pane that asks the theme for a role no palette defines.
--
-- `lib/theme.lua`'s `__index` returns nil for an unknown shorthand, so the run
-- is drawn in the terminal's default colour and looks deliberate.
local theme = require("lib.theme")

return {
  name = "theme_probe",
  render = function()
    return { text = "warning", style = { fg = theme.warning } }
  end,
}
