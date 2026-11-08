-- A pane that misspells a node prop: `txet` where the kind needs `text`.
--
-- `convert.rs` drops a key it does not know — that is how a plugin carries its
-- own bookkeeping on the node table — so this renders an empty line at runtime
-- and says nothing. The type definitions are what turn it into a finding.
return {
  name = "prop_probe",
  render = function()
    ---@type thurbox.TextNode
    local row = { type = "text", txet = "hello" }
    return row
  end,
}
