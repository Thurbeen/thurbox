-- The session list's model: what to draw, before any width is known.
--
-- Pure functions over the published snapshot tables — sessions and in-flight
-- commands in, an ordered item list out. An item is one selectable unit — a
-- session row plus, for a group's first row, the group header glued on top.
-- That gluing is v1's: the header is line zero of the first session's list
-- item, so clicking a header selects that session and the scroll window can
-- never separate the two.
--
-- Deliberately free of theme and widgets: nothing here is text yet. The one
-- dependency is `lib.settings`, because whether headers are drawn is a knob
-- the model has to answer for every consumer at once.

local plugin_settings = require("lib.settings")

local session_model = {}

local NO_REPO = "(no repo)"

--- In-flight commands, keyed by the session they concern.
---
--- A command is accepted instantly and lands in a later snapshot, so without
--- this a restart would look like nothing happened for a moment. v1 needed a
--- whole `PendingSpawn` type for the same reason. A delete is the exception:
--- see `live_sessions()`, which drops the row instead of annotating it.
function session_model.pending()
  local by_session = {}
  for _, item in ipairs(thurbox and thurbox.commands or {}) do
    if item.session and item.session ~= "" then
      by_session[item.session] = item
    end
  end
  return by_session
end

--- The sessions the list draws: every published row but the ones being deleted.
---
--- Every other in-flight command leaves a row to annotate; a delete is the one
--- whose subject is the row itself. Waiting for it to land left the session
--- sitting there tagged `delete` for as long as the worker took — so the list
--- kept showing what you had just removed. Dropping it on the keystroke is what
--- makes the delete read as done, and there is nothing to be lost by it: the
--- effect is already accepted, and Ctrl+Z restores the session rather than the
--- row.
---
--- A FAILED delete is deliberately kept. The session is still there, and the
--- failed row is the only thing that says the deletion did not happen.
local function live_sessions(rows)
  local gone = {}
  for _, item in ipairs(thurbox and thurbox.commands or {}) do
    -- Guarded like `pending()`: a nil key is a runtime error in Lua.
    if item.kind == "delete" and item.phase ~= "failed" and item.session then
      gone[item.session] = true
    end
  end

  local live = {}
  for _, session in ipairs(rows) do
    if not gone[session.id] then
      live[#live + 1] = session
    end
  end
  return live
end

--- Creations in flight, keyed by the repo they will land in.
---
--- A create names no session yet, so it cannot be matched to a row. The command
--- carries its subject — the repo — which is exactly enough to draw the
--- placeholder where the session will actually appear rather than in a limbo of
--- its own. v1 needed a bespoke slot in its ordering code for this.
local function pending_creations()
  local by_repo = {}
  for _, item in ipairs(thurbox and thurbox.commands or {}) do
    if item.kind == "create" and item.subject then
      by_repo[item.subject] = by_repo[item.subject] or {}
      table.insert(by_repo[item.subject], item)
    end
  end
  return by_repo
end

--- The repos a session spans, de-duplicated, in its own member order — primary
--- repo first. Empty when it spans none.
local function repo_set(session)
  local seen, names = {}, {}
  for _, name in ipairs(session.repos or {}) do
    if name ~= "" and not seen[name] then
      seen[name] = true
      names[#names + 1] = name
    end
  end
  -- A row published before the member list existed still has to group.
  if #names == 0 and session.repo then
    names[1] = session.repo
  end
  return names
end

--- The grouping **key**: the repo *set*, sorted, so two sessions spanning the
--- same repos cluster regardless of the order they were selected in. Mirrors
--- v1's `repo_set_key` — including its `\0` separator, which cannot occur in a
--- repo name, so distinct sets never collide. Never displayed.
local function group_key(names)
  if #names == 0 then
    return NO_REPO
  end
  local sorted = {}
  for index, name in ipairs(names) do
    sorted[index] = name
  end
  table.sort(sorted)
  return table.concat(sorted, "\0")
end

--- The header **label**: the same repos joined with ` + ` in natural order, so a
--- multi-repo session's group names every repo it spans rather than just its
--- primary. v1's `repo_set_display`.
local function group_label(names)
  if #names == 0 then
    return NO_REPO
  end
  return table.concat(names, " + ")
end

--- Group by repo set and order exactly as v1's `compute_session_order` does:
--- members by (manual order, original index), groups by (lowest member order,
--- label). "Never moved" sorts *after* everything ordered, in creation order —
--- not alphabetically.
local function ordered_groups(rows)
  local groups, by_key = {}, {}
  for index, session in ipairs(rows) do
    local names = repo_set(session)
    local key = group_key(names)
    local group = by_key[key]
    if not group then
      group = { label = group_label(names), members = {} }
      by_key[key] = group
      groups[#groups + 1] = group
    end
    table.insert(group.members, index)
  end

  local function manual(index)
    return rows[index].display_order or math.huge
  end

  for _, group in ipairs(groups) do
    table.sort(group.members, function(a, b)
      local left, right = manual(a), manual(b)
      if left ~= right then
        return left < right
      end
      return a < b
    end)
    local lowest = math.huge
    for _, index in ipairs(group.members) do
      lowest = math.min(lowest, manual(index))
    end
    group.order = lowest
  end

  table.sort(groups, function(a, b)
    if a.order ~= b.order then
      return a.order < b.order
    end
    return a.label < b.label
  end)
  return groups, by_key
end

--- Whether the list draws repo headers.
---
--- v1 always groups; a user with one repo sees a header that says nothing, so
--- this is a knob rather than a rule. The GROUPING still happens either way --
--- only the header line is suppressed -- so ordering and the move-past-a-group
--- behaviour are unchanged.
local function grouped()
  return plugin_settings.enabled("sessions", "group_by_repo", true)
end

--- Digest of the published in-flight commands, for the model memo below.
---
--- `thurbox.sessions` is a gated group, so its table identity is a sound memo
--- key — but `thurbox.commands` is rebuilt every publish, so it is digested by
--- value instead. Commands in flight are few, so the digest is far cheaper
--- than the rebuild it prevents.
local function commands_digest()
  local parts = {}
  for _, item in ipairs(thurbox and thurbox.commands or {}) do
    parts[#parts + 1] = (item.kind or "")
      .. "\1"
      .. (item.session or "")
      .. "\1"
      .. (item.subject or "")
      .. "\1"
      .. (item.phase or "")
  end
  return table.concat(parts, "\2")
end

local model_cache = {}

--- The rows to draw, in order, before any of them is turned into text.
function session_model.build(rows)
  -- Memoized: this walks and sorts every row and runs again per render AND per
  -- click/action, so the same inputs must not pay twice. Consumers treat the
  -- returned items as read-only, which is what makes sharing the table safe.
  local digest = commands_digest()
  local headers = grouped()
  if
    model_cache.items ~= nil
    and rawequal(rows, model_cache.rows)
    and digest == model_cache.digest
    and headers == model_cache.headers
  then
    return model_cache.items
  end

  local items = {}
  -- Dropped before anything is grouped or ordered, so every consumer of the
  -- model agrees: the cursor lands on the next row, the border dots lose one,
  -- and a group whose last session went takes its header with it.
  local all_rows = rows
  rows = live_sessions(rows)

  local groups, by_key = ordered_groups(rows)
  local creating = pending_creations()

  -- A repo that has no sessions yet still needs its header, or a creation into
  -- a fresh repo would have nowhere to draw.
  for repo in pairs(creating) do
    if not by_key[repo] then
      local group = { label = repo, members = {}, order = math.huge }
      by_key[repo] = group
      groups[#groups + 1] = group
    end
  end

  -- Every rendered session, for the cross-group child mark: v1 only marks a
  -- child whose parent is actually on screen somewhere.
  local visible = {}
  for _, session in ipairs(rows) do
    visible[session.id] = true
  end

  for _, group in ipairs(groups) do
    local in_group = {}
    for _, index in ipairs(group.members) do
      in_group[rows[index].id] = true
    end

    -- Children nest under their parent, within the same repo group, keeping the
    -- manual order among siblings and among roots. Indexed by parent up front:
    -- scanning the member list per emitted member made the walk O(members²).
    local children = {}
    for _, index in ipairs(group.members) do
      local parent = rows[index].parent
      if parent then
        children[parent] = children[parent] or {}
        table.insert(children[parent], index)
      end
    end

    local nested, seen = {}, {}
    local function emit(index, depth)
      local session = rows[index]
      if seen[session.id] then
        return
      end
      seen[session.id] = true
      nested[#nested + 1] = { index = index, depth = depth }
      for _, other in ipairs(children[session.id] or {}) do
        emit(other, depth + 1)
      end
    end
    for _, index in ipairs(group.members) do
      local session = rows[index]
      if not session.parent or not in_group[session.parent] then
        emit(index, 0)
      end
    end

    local first = true
    for _, entry in ipairs(nested) do
      local session = rows[entry.index]
      local parent = session.parent
      items[#items + 1] = {
        kind = "session",
        session = session,
        depth = entry.depth,
        cross_group = entry.depth == 0
          and parent ~= nil
          and parent ~= session.id
          and visible[parent] == true,
        header = (first and headers) and group.label or nil,
        target = session.id,
      }
      first = false
    end

    -- Placeholders at the end of the group, where the real row will appear.
    for _, item in ipairs(creating[group.label] or {}) do
      items[#items + 1] = {
        kind = "pending",
        command = item,
        -- A placeholder sits at group level, like the row it will become. Stated
        -- rather than left nil because every ordering helper compares `depth`
        -- numerically, and `nil` there is not a shallow row -- it is an error that
        -- takes the pane down on Shift+J/K/S while a session is being created.
        depth = 0,
        header = (first and headers) and group.label or nil,
        target = false,
      }
      first = false
    end
  end

  model_cache.rows = all_rows
  model_cache.digest = digest
  model_cache.headers = headers
  model_cache.items = items
  return items
end

return session_model
