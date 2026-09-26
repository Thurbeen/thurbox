-- Typing a filesystem path, with a listing to lean on.
--
-- The creation flow's path input has three derived answers — which directory
-- to list, which listed entries the typed prefix keeps, and the one ghost
-- completion that extends what was typed. They are shared by the renderer, the
-- key handler and the completion on purpose: separate derivations of "which
-- entries" would eventually disagree about which one `enter` picks.
--
-- Pure functions over the typed string and the listing the kernel already
-- served — nothing here reads the snapshot or asks for anything.

local pathpicker = {}

--- Split a typed path into the directory to list and the prefix to filter by.
---
--- v1's `split_browse_dir`, and used for both jobs it has there: the directory is
--- what gets listed, the prefix is what the ghost completion extends. With no `/`
--- at all the home directory is listed and the whole input is the prefix.
function pathpicker.split_typed(typed)
  if typed == "~" then
    -- A bare `~` means "browse home", not "filter home by the literal ~" — v1
    -- makes the same exception.
    return "~", ""
  end
  local dir = typed:match("^(.*)/[^/]*$")
  if dir == "" then
    dir = "/"
  end
  return dir or "~", typed:match("([^/]*)$") or ""
end

--- The dropdown's entries: `listed` filtered by what has been typed after the
--- last `/`.
---
--- v1's `PathBrowser::recompute_filter`, including its rule for hidden entries —
--- a `.`-prefixed name is offered only when the prefix itself starts with a dot,
--- so browsing a home directory is not two hundred dotfiles.
function pathpicker.entries(typed, listed)
  local _, prefix = pathpicker.split_typed(typed or "")
  local shown = {}
  for _, entry in ipairs(listed or {}) do
    local hidden = entry.name:sub(1, 1) == "." and prefix:sub(1, 1) ~= "."
    if not hidden and entry.name:sub(1, #prefix) == prefix then
      shown[#shown + 1] = entry
    end
  end
  return shown
end

--- The ghost completion: the one listed entry that extends what has been typed.
---
--- Derived from the listing already fetched for the dropdown, so it costs a table
--- walk rather than v1's synchronous readdir per keystroke — and unlike v1's it
--- works for a remote target, where v1 suppresses completion entirely because it
--- would be completing against the wrong filesystem.
function pathpicker.suggestion(typed, listed)
  typed = typed or ""
  local _, prefix = pathpicker.split_typed(typed)
  if typed == "" or prefix == "" then
    return ""
  end
  local shown = pathpicker.entries(typed, listed)
  -- Two candidates is no suggestion: completing to one of them would be a guess,
  -- and v1 makes the same call.
  if #shown ~= 1 then
    return ""
  end
  return shown[1].name:sub(#prefix + 1) .. "/"
end

--- Where a new path most likely goes: the deepest directory that holds every
--- remembered repository.
---
--- Compared by whole components, over each repository's PARENT — so one
--- repository answers its own parent, and `/src/app/x` beside `/src/apple/y`
--- answers `/src` rather than the character prefix `/src/app`. Folder headers
--- are folders rather than repositories in one, and an `offered` row is the
--- kernel's suggestion rather than the user's memory; neither says where the
--- user keeps their code. With nothing to go on the answer is home.
function pathpicker.common_root(rows)
  local common, absolute
  for _, row in ipairs(rows or {}) do
    if not row.is_parent and not row.offered and row.path then
      local parts = {}
      for part in row.path:gmatch("[^/]+") do
        parts[#parts + 1] = part
      end
      parts[#parts] = nil
      local rooted = row.path:sub(1, 1) == "/"
      if not common then
        common, absolute = parts, rooted
      elseif rooted ~= absolute then
        -- `~/x` beside `/x` share nothing a string can say.
        return "~"
      else
        local shared = 0
        while shared < #common and common[shared + 1] == parts[shared + 1] do
          shared = shared + 1
        end
        for index = #common, shared + 1, -1 do
          common[index] = nil
        end
      end
    end
  end
  if not common then
    return "~"
  end
  local joined = table.concat(common, "/")
  if absolute then
    return "/" .. joined
  end
  return joined ~= "" and joined or "~"
end

--- The path field's starting value: `common_root` as a directory to type under.
function pathpicker.start(rows)
  local root = pathpicker.common_root(rows)
  return root == "/" and "/" or (root .. "/")
end

return pathpicker
