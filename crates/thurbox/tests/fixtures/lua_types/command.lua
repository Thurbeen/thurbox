return {
  name = "command_probe",
  render = function()
    return { text = "" }
  end,
  on_action = function()
    command("open", { url = "https://example.invalid" })
  end,
}
