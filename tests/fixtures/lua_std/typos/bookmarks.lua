-- `thurbox.bookmarks`, with one letter wrong.
return {
  name = "std_typo_bookmarks",
  render = function()
    return { text = tostring(thurbox.bookmarks.rowz) }
  end,
}
