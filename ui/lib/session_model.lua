local mod_settings = require("lib.settings")

local session_model = {}

local NO_REPO = "(no repo)"

local FLAT_KEY = "\0flat"

local LABEL_KEY = "\0group\0"

local function labelled_group(session)
  local label = session.meta and session.meta["ui.group"]
  label = label and label:match("^%s*(.-)%s*$")
  if label == "" then
    return nil
  end
  return label
end

local LOCAL_HOST_KEY = "\0local"

local function group_slot(host, label)
  return (host or "") .. "\1" .. label
end

local function host_of(session)
  return session.host or LOCAL_HOST_KEY
end

local function spans_hosts(rows, creations)
  local seen, count = {}, 0
  local function note(host)
    if not seen[host] then
      seen[host] = true
      count = count + 1
    end
    return count > 1
  end
  for _, session in ipairs(rows) do
    if note(host_of(session)) then
      return true
    end
  end
  for _, item in ipairs(creations) do
    if note(item.host or LOCAL_HOST_KEY) then
      return true
    end
  end
  return false
end

function session_model.pending()
  local by_session = {}
  for _, item in ipairs(thurbox and thurbox.commands or {}) do
    if item.session and item.session ~= "" then
      by_session[item.session] = item
    end
  end
  return by_session
end

local function live_sessions(rows)
  local gone = {}
  for _, item in ipairs(thurbox and thurbox.commands or {}) do
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

local function creations()
  local all = {}
  for _, item in ipairs(thurbox and thurbox.commands or {}) do
    if item.kind == "create" and item.subject then
      all[#all + 1] = item
    end
  end
  return all
end

local function pending_creations(all, by_host)
  local by_slot, buckets = {}, {}
  for _, item in ipairs(all) do
    local host = by_host and (item.host or LOCAL_HOST_KEY) or nil
    local slot = group_slot(host, item.subject)
    local bucket = by_slot[slot]
    if not bucket then
      bucket = { host = host, repo = item.subject, items = {} }
      by_slot[slot] = bucket
      buckets[#buckets + 1] = bucket
    end
    table.insert(bucket.items, item)
  end
  return by_slot, buckets
end

local function repo_set(session)
  local seen, names = {}, {}
  for _, name in ipairs(session.repos or {}) do
    if name ~= "" and not seen[name] then
      seen[name] = true
      names[#names + 1] = name
    end
  end
  if #names == 0 and session.repo then
    names[1] = session.repo
  end
  return names
end

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

local function group_label(names)
  if #names == 0 then
    return NO_REPO
  end
  return table.concat(names, " + ")
end

local function ordered_groups(rows, grouping, by_host)
  local groups, by_key = {}, {}
  for index, session in ipairs(rows) do
    local key, label
    local named = grouping and labelled_group(session)
    if named then
      key, label = LABEL_KEY .. named, named
    elseif grouping then
      local names = repo_set(session)
      key, label = group_key(names), group_label(names)
    else
      key, label = FLAT_KEY, NO_REPO
    end
    local repo_key = key
    local host = by_host and host_of(session) or nil
    if host then
      key = host .. "\1" .. key
    end
    local group = by_key[key]
    if not group then
      group = { label = label, repo_key = repo_key, host = host, members = {} }
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
    if a.label ~= b.label then
      return a.label < b.label
    end
    return (a.host or "") < (b.host or "")
  end)
  return groups
end

local function by_host_first(groups)
  local position = {}
  for index, group in ipairs(groups) do
    position[group] = index
  end
  table.sort(groups, function(a, b)
    if a.host ~= b.host then
      local a_remote = a.host ~= LOCAL_HOST_KEY
      local b_remote = b.host ~= LOCAL_HOST_KEY
      if a_remote ~= b_remote then
        return b_remote
      end
      return a.host < b.host
    end
    return position[a] < position[b]
  end)
  return groups
end

local function header_label(group, grouping, by_host, host_rows)
  if by_host and not host_rows then
    local machine = group.host == LOCAL_HOST_KEY and "local" or group.host
    return grouping and (machine .. " · " .. group.label) or machine
  end
  return grouping and group.label or nil
end

local function grouped()
  return mod_settings.enabled("sessions", "group_by_repo", true)
end

local function host_grouped()
  return mod_settings.enabled("sessions", "group_by_host", true)
end

local function commands_digest()
  local parts = {}
  for _, item in ipairs(thurbox and thurbox.commands or {}) do
    parts[#parts + 1] = (item.kind or "")
      .. "\1"
      .. (item.session or "")
      .. "\1"
      .. (item.subject or "")
      .. "\1"
      .. (item.host or "")
      .. "\1"
      .. (item.phase or "")
  end
  return table.concat(parts, "\2")
end

local function decode_set(saved)
  local set = {}
  for escaped in saved:gmatch("[^;]+") do
    set[escaped:gsub("%%(%x%x)", function(hex)
      return string.char(tonumber(hex, 16))
    end)] =
      true
  end
  return set
end

local model_cache = {}

function session_model.build(rows, folded, reveal, host_rows, folded_repos, folded_parents)
  local digest = commands_digest()
  local grouping = grouped()
  local host_grouping = host_grouped()
  host_rows = host_rows == true
  folded = folded or ""
  folded_repos = folded_repos or ""
  folded_parents = folded_parents or ""
  if
    model_cache.items ~= nil
    and rawequal(rows, model_cache.rows)
    and digest == model_cache.digest
    and grouping == model_cache.grouping
    and host_grouping == model_cache.host_grouping
    and host_rows == model_cache.host_rows
    and folded == model_cache.folded
    and folded_repos == model_cache.folded_repos
    and folded_parents == model_cache.folded_parents
    and reveal == model_cache.reveal
  then
    return model_cache.items
  end

  local items = {}
  local folded_parent = decode_set(folded_parents)
  local all_rows = rows
  rows = live_sessions(rows)

  local all_creating = creations()
  local by_host = host_grouping
    and (host_rows and (#rows > 0 or #all_creating > 0) or spans_hosts(rows, all_creating))
  local groups = ordered_groups(rows, grouping, by_host)
  local creating, creating_buckets = pending_creations(all_creating, by_host)

  if grouping then
    local have = {}
    for _, group in ipairs(groups) do
      have[group_slot(group.host, group.repo_key)] = true
    end
    for _, bucket in ipairs(creating_buckets) do
      local slot = group_slot(bucket.host, bucket.repo)
      if not have[slot] then
        have[slot] = true
        groups[#groups + 1] = {
          label = bucket.repo,
          repo_key = bucket.repo,
          host = bucket.host,
          members = {},
          order = math.huge,
        }
      end
    end
  elseif #all_creating > 0 then
    local have = {}
    for _, group in ipairs(groups) do
      have[group.host or ""] = true
    end
    for _, bucket in ipairs(creating_buckets) do
      if not have[bucket.host or ""] then
        have[bucket.host or ""] = true
        groups[#groups + 1] = {
          label = NO_REPO,
          repo_key = FLAT_KEY,
          host = bucket.host,
          members = {},
          order = math.huge,
        }
      end
    end
  end

  if by_host then
    by_host_first(groups)
  end

  for _, group in ipairs(groups) do
    if grouping then
      local bucket = creating[group_slot(group.host, group.repo_key)]
      group.placeholders = bucket and bucket.items or nil
    else
      local mine = {}
      for _, item in ipairs(all_creating) do
        local host = by_host and (item.host or LOCAL_HOST_KEY) or nil
        if host == group.host then
          mine[#mine + 1] = item
        end
      end
      group.placeholders = mine
    end
  end

  local visible = {}
  for _, session in ipairs(rows) do
    visible[session.id] = true
  end

  for _, group in ipairs(groups) do
    local placeholders = group.placeholders or {}
    local group_header = nil
    if grouping or (by_host and not host_rows) then
      group_header = header_label(group, grouping, by_host, host_rows)
    end

    local in_group = {}
    for _, index in ipairs(group.members) do
      in_group[rows[index].id] = true
    end

    local children = {}
    for _, index in ipairs(group.members) do
      local parent = rows[index].parent
      if parent then
        children[parent] = children[parent] or {}
        table.insert(children[parent], index)
      end
    end

    local nested, seen = {}, {}
    local function emit(index, depth, hidden, ancestor)
      local session = rows[index]
      if seen[session.id] then
        return 0
      end
      seen[session.id] = true
      local shown = not hidden or session.status == "blocked"
      local entry = { index = index, depth = depth, hidden = not shown, visible_parent = ancestor }
      nested[#nested + 1] = entry
      local folded_here = folded_parent[session.id] == true and not reveal
      local descendants = 0
      for _, other in ipairs(children[session.id] or {}) do
        descendants = descendants
          + 1
          + emit(other, depth + 1, hidden or folded_here, shown and session.id or ancestor)
      end
      entry.descendants = descendants
      entry.collapsed = folded_here and descendants > 0
      return descendants
    end
    for _, index in ipairs(group.members) do
      local session = rows[index]
      if not session.parent or not in_group[session.parent] then
        emit(index, 0, false)
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
        descendants = entry.descendants,
        collapsed = entry.collapsed,
        hidden = entry.hidden,
        visible_parent = entry.visible_parent,
        cross_group = entry.depth == 0
          and parent ~= nil
          and parent ~= session.id
          and visible[parent] == true,
        header = first and group_header or nil,
        host = group.host,
        repo_label = group.label,
        repo_target = ":repo:" .. group_slot(group.host, group.repo_key),
        group_size = #group.members,
        target = session.id,
      }
      first = false
    end

    for _, item in ipairs(placeholders) do
      items[#items + 1] = {
        kind = "pending",
        command = item,
        depth = 0,
        header = first and group_header or nil,
        host = group.host,
        repo_label = group.label,
        repo_target = ":repo:" .. group_slot(group.host, group.repo_key),
        target = false,
      }
      first = false
    end
  end

  if by_host and host_rows then
    local collapsed = decode_set(folded)
    local counts = {}
    for _, item in ipairs(items) do
      if item.host then
        local host = item.host
        local count = counts[host]
          or {
            total = 0,
            working = 0,
            idle = 0,
            attention = 0,
            unreachable = 0,
            failed = 0,
          }
        local status = item.session and item.session.status or nil
        if item.session then
          count.backend = count.backend or item.session.backend
          count.total = count.total + 1
          if status == "working" then
            count.working = count.working + 1
          elseif status == "idle" or status == "stopped" then
            count.idle = count.idle + 1
          elseif
            status == "blocked"
            or status == "done"
            or status == "error"
            or status == "unreachable"
          then
            count.attention = count.attention + 1
          end
          if status == "unreachable" then
            count.unreachable = count.unreachable + 1
          end
        elseif item.command and item.command.phase == "failed" then
          count.attention = count.attention + 1
          count.failed = count.failed + 1
        end
        counts[host] = count
      end
    end
    local with_hosts, seen = {}, {}
    for _, item in ipairs(items) do
      local host = item.host
      if host then
        if not seen[host] then
          seen[host] = true
          with_hosts[#with_hosts + 1] = {
            kind = "host",
            host = host,
            counts = counts[host],
            collapsed = collapsed[host] == true,
            target = ":host:" .. host,
          }
        end
        if not collapsed[host] or reveal then
          with_hosts[#with_hosts + 1] = item
        end
      else
        with_hosts[#with_hosts + 1] = item
      end
    end
    items = with_hosts
  end

  local shown = {}
  for _, item in ipairs(items) do
    if not item.hidden then
      shown[#shown + 1] = item
    end
  end
  items = shown

  if host_rows and grouping then
    local collapsed = decode_set(folded_repos)
    local with_repos, seen = {}, {}
    for _, item in ipairs(items) do
      local target = item.repo_target
      if target then
        if not seen[target] then
          seen[target] = true
          with_repos[#with_repos + 1] = {
            kind = "repo",
            target = target,
            host = item.host,
            repo_label = item.repo_label,
            count = item.group_size or 0,
            collapsed = collapsed[target] == true,
          }
        end
        if not collapsed[target] or reveal then
          with_repos[#with_repos + 1] = item
        end
      else
        with_repos[#with_repos + 1] = item
      end
    end
    items = with_repos
  end

  model_cache.rows = all_rows
  model_cache.digest = digest
  model_cache.grouping = grouping
  model_cache.host_grouping = host_grouping
  model_cache.host_rows = host_rows
  model_cache.folded = folded
  model_cache.folded_repos = folded_repos
  model_cache.folded_parents = folded_parents
  model_cache.reveal = reveal
  model_cache.items = items
  return items
end

---@param session thurbox.Session
---@return "start"|"recreate"|nil
function session_model.revival(session)
  if session.status == "stopped" then
    return "start"
  end
  local why = session.state_why or ""
  if why == "lost · cannot resume" or why:find("^gone") then
    return "recreate"
  end
  if why:find("^lost") then
    return "start"
  end
  return nil
end

return session_model
