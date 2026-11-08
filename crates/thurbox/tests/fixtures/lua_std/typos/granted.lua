-- `thurbox.granted`, with one letter wrong.
--
-- One table per file so the assertion is the file name plus a diagnostic code,
-- never the wording of a message. See `scripts/ci/check-lua-std.sh`.
return {
  name = "std_typo_granted",
  render = function()
    return { text = tostring(thurbox.granted.prgoram) }
  end,
}
