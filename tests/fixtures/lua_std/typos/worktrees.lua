-- `thurbox.worktrees`, with one letter wrong.
return {
  name = "std_typo_worktrees",
  render = function()
    return { text = tostring(thurbox.worktrees.lst) }
  end,
}
