-- A pane that names a command option the verb does not read.
--
-- `command("open", …)` takes its url in `text`; `url` is collected into the
-- payload and ignored, so the link never opens and nothing is reported.
return {
  name = "command_probe",
  render = function()
    return { text = "" }
  end,
  on_action = function()
    command("open", { url = "https://example.invalid" })
  end,
}
