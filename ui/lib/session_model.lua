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
-- dependency is `lib.settings`, because whether the list groups by repo is a
-- knob the model has to answer for every consumer at once.
--
-- Two axes, one group: the repo set a session spans, and — only when the rows
-- span more than one machine — the host it runs on, which is the outer one.

local plugin_settings = require("lib.settings")

local session_model = {}

local NO_REPO = "(no repo)"

--- The single group's key when grouping is off. `\0` cannot occur in a repo
--- name — the same property `group_key` relies on — so it can never collide
--- with a real repo set.
local FLAT_KEY = "\0flat"

--- What the host axis calls a session running on this machine — the grouping
--- KEY, and separately the word a header shows for it. A local row has no host
--- name to show and its group still has to name the machine the remote ones are
--- named against, so the word stands in for one.
---
--- The key is `\0`-prefixed, like `FLAT_KEY` and for the same reason: a host
--- name cannot contain `\0`, so a host somebody actually called `local` is a
--- second machine here rather than one merged into this one's group. Spelling
--- the key as the bare word merged the two silently, and — because `lib.order`
--- decides a host boundary by comparing these keys — it also turned off the
--- refusal that keeps a group from being moved onto another machine.
local LOCAL_HOST_KEY = "\0local"
local LOCAL_HOST_LABEL = "local"

--- A group's identity across the two axes, for the lookups below that have to
--- ask "does THIS machine already have a group for that repo?".
local function group_slot(host, label)
  return (host or "") .. "\1" .. label
end

--- The two axes in one header line, so a group names its machine and its repos
--- at once. Two header LEVELS would be a second thing `lib/order`'s move and
--- sort algebra has to understand — it finds a group's edges by looking for the
--- single row that carries a header — and the nesting would have to be taught
--- to the click targets and the border dots as well. A qualified label is the
--- same information in the shape everything downstream already reads.
local HOST_SEPARATOR = " · "

--- Which machine a session runs on. `session.host` is the bare remote host name
--- and nil when the session is local, so this is the one place the two are one
--- vocabulary.
local function host_of(session)
  return session.host or LOCAL_HOST_KEY
end

--- Does this list span more than one machine?
---
--- Half the gate on the host axis, and the half that is not a preference: one
--- machine — a laptop with no remote sessions, and equally a list whose every
--- session is on the same remote box — means the keys, the labels and the order
--- below are what they were before this axis existed. A header naming the only
--- machine on screen is noise whatever the operator asked for, so it is asked
--- even when the switch is on. The other half is that switch, which answers
--- whether the operator wants their machines separated at all.
---
--- A creation in flight counts, and it has to: creating your FIRST session on a
--- host is a list whose rows are all local and whose next row is not. Counting
--- rows alone, the axis stayed off, the machine the creation named was dropped,
--- and the placeholder sat under a header that named no machine until the row
--- landed and the whole list regrouped underneath it.
local function spans_hosts(rows, creations)
  local seen, count = {}, 0
  local function note(host)
    if seen[host] then
      return false
    end
    seen[host] = true
    count = count + 1
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

--- Every creation in flight, in publication order.
---
--- Read before the host axis is decided, because deciding it counts the machines
--- these name as well as the machines the rows are on.
---
--- A FAILED create is kept, unlike the failed delete `live_sessions` drops: the
--- row it would have made does not exist, so its placeholder is the only thing
--- that says the creation happened at all, and it stays for the few seconds the
--- bus holds a failure. It therefore also holds the host axis on for that long,
--- which is the point rather than a side effect — the failure is reported under
--- the machine it was asked for.
local function creations()
  local all = {}
  for _, item in ipairs(thurbox and thurbox.commands or {}) do
    if item.kind == "create" and item.subject then
      all[#all + 1] = item
    end
  end
  return all
end

--- Those creations bucketed by the group each will land in, and the buckets in
--- publication order.
---
--- A create names no session yet, so it cannot be matched to a row. It carries
--- the two facts that identify its group instead: its subject — the repo — and,
--- once the host axis is on, the machine it was asked for. v1 needed a bespoke
--- slot in its ordering code for this.
---
--- `item.host` is nil for a creation on this machine, which is the same thing
--- `session.host` says about a local row, so the two axes read the same either
--- side of a session existing. The bucket list is ordered because `pairs` over
--- the map is not: two fresh repos created at once would otherwise swap headers
--- between runs.
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
---
--- With `grouping` off there is exactly one group holding every row, so the
--- manual order is the whole order. See `grouped()` for why off has to mean
--- that rather than the same clustering with its headers hidden.
---
--- `by_host` splits each of those groups per machine. It only qualifies the KEY
--- here; clustering the machines together is `by_host_first`, so the order
--- above stays the one v1 computes and the host is layered over it.
local function ordered_groups(rows, grouping, by_host)
  local groups, by_key = {}, {}
  for index, session in ipairs(rows) do
    local key, label
    if grouping then
      local names = repo_set(session)
      key, label = group_key(names), group_label(names)
    else
      key, label = FLAT_KEY, NO_REPO
    end
    -- `\1` rather than the `\0` the repo-set key already separates with: a
    -- host prefixed with the same byte would read as one more repo in the set,
    -- and a host and a repo sharing a name would then collide.
    local repo_key = key
    local host = by_host and host_of(session) or nil
    if host then
      key = host .. "\1" .. key
    end
    local group = by_key[key]
    if not group then
      -- `repo_key` and not `label`: a session spanning `a` and `b` is labelled
      -- `a + b`, and so is a session in a repo directory literally called
      -- `a + b`. Their keys differ, and a creation names a repo, so matching a
      -- placeholder on the key puts it in the group it is actually for.
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
    -- Two machines running the same repos give two groups one label, and
    -- `table.sort` is not stable — so without the host they would swap places
    -- between builds. Both hosts are nil when the axis is off, and the
    -- comparison then answers exactly what the label alone answered.
    if a.label ~= b.label then
      return a.label < b.label
    end
    return (a.host or "") < (b.host or "")
  end)
  return groups
end

--- The same groups, re-clustered so every group of one machine is adjacent:
--- this machine first, then each remote host by name. Stable within a host, so
--- the manual order this function is handed survives inside it.
---
--- Applied only when the rows span more than one machine, which is what keeps a
--- single-host list byte-for-byte what it was.
local function by_host_first(groups)
  local position = {}
  for index, group in ipairs(groups) do
    position[group] = index
  end
  table.sort(groups, function(a, b)
    if a.host ~= b.host then
      -- This machine first: it is the one you are sitting at, and on a list
      -- that is mostly local it is the one you are mostly reading.
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

--- What a group's header says: the repos, the machine, or both.
local function header_label(group, grouping, by_host)
  if not by_host then
    return group.label
  end
  local machine = group.host
  if machine == LOCAL_HOST_KEY then
    machine = LOCAL_HOST_LABEL
  end
  if not grouping then
    return machine
  end
  return machine .. HOST_SEPARATOR .. group.label
end

--- Whether the list groups by repo.
---
--- v1 always groups; a user with one repo sees a header that says nothing, so
--- this is a knob rather than a rule. Off means genuinely UNGROUPED, not the
--- same clustering with its headers hidden: one flat list ordered by the manual
--- order alone. Hiding only the header line is what this was first, and it made
--- a move ACROSS repos persist and then be undone by the next build
--- re-clustering the row under its own repo -- with the headers that would have
--- explained it turned off. Parent/child nesting is unaffected either way; it
--- is not a repo property.
local function grouped()
  return plugin_settings.enabled("sessions", "group_by_repo", true)
end

--- Whether the list separates the machines it spans.
---
--- The second half of the host gate, beside `spans_hosts`. The axis shipped
--- derived from the machine count alone, so an operator who wants the remote
--- sessions in among the local ones -- one list, ordered by hand across the
--- lot -- had no way to say so. On by default, so a list that already draws
--- host headers keeps drawing them.
---
--- A second boolean rather than one `none / repo / host / host then repo` row
--- because the settings modal has no choice type: a plugin's `Text` value is
--- free text you type into, and a misspelt word there would silently mean
--- "none". The two are independent anyway — all four combinations render, from
--- one flat list to a header naming a machine and its repos.
local function host_grouped()
  return plugin_settings.enabled("sessions", "group_by_host", true)
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
      -- The machine a creation names is read by the model, so two creations
      -- differing only in it are two different lists. A create's `session` is
      -- always empty, so without this they digest the same and the second one
      -- is drawn with the first one's grouping.
      .. (item.host or "")
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
  local grouping = grouped()
  -- Both knobs in the key. `by_host` below is derived from the rows too, and
  -- those are covered by the two entries above it -- but the switch is read
  -- from the registry, which neither the row table nor the digest can see, so
  -- without it flipping the row in the modal is answered from the cache.
  local host_grouping = host_grouped()
  if
    model_cache.items ~= nil
    and rawequal(rows, model_cache.rows)
    and digest == model_cache.digest
    and grouping == model_cache.grouping
    and host_grouping == model_cache.host_grouping
  then
    return model_cache.items
  end

  local items = {}
  -- Dropped before anything is grouped or ordered, so every consumer of the
  -- model agrees: the cursor lands on the next row, the border dots lose one,
  -- and a group whose last session went takes its header with it.
  local all_rows = rows
  rows = live_sessions(rows)

  local all_creating = creations()
  -- Short-circuited on the switch: with the axis off there is nothing for the
  -- tally to decide, and every group below is built the way it was before the
  -- axis existed.
  local by_host = host_grouping and spans_hosts(rows, all_creating)
  local groups = ordered_groups(rows, grouping, by_host)
  local creating, creating_buckets = pending_creations(all_creating, by_host)

  -- A creation has no row yet, so the group that draws it is built for it when
  -- nothing on screen is already that group: the repo it names, on the machine
  -- it was asked for. Both halves matter — keyed by the repo alone, a creation
  -- into a repo only a remote host holds drew its placeholder over there and
  -- said the session was being spun up on that box.
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
    -- Ungrouped: one flat group per machine, and a creation still needs its
    -- machine's — which may hold no session at all, and on a list with nothing
    -- on it yet is the only group there is.
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

  -- Handed to the group built for it above. No claiming: a bucket is keyed by
  -- (machine, repo key) and so is a group, and every bucket either matched a
  -- group or had one built, so each creation reaches exactly one.
  for _, group in ipairs(groups) do
    if grouping then
      local bucket = creating[group_slot(group.host, group.repo_key)]
      group.placeholders = bucket and bucket.items or nil
    else
      -- One group per machine, so it takes every creation for that machine,
      -- in publication order.
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

  -- Every rendered session, for the cross-group child mark: v1 only marks a
  -- child whose parent is actually on screen somewhere.
  local visible = {}
  for _, session in ipairs(rows) do
    visible[session.id] = true
  end

  for _, group in ipairs(groups) do
    -- Claimed above: grouped, a placeholder belongs to the repo it names;
    -- ungrouped there is one list per machine and it lands at the end of the
    -- first.
    local placeholders = group.placeholders or {}
    -- Composed once per group rather than per row: `first` decides WHETHER a
    -- row carries it, this decides what it says.
    local group_header = nil
    if grouping or by_host then
      group_header = header_label(group, grouping, by_host)
    end

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
        header = first and group_header or nil,
        -- Which machine this row is on, or nil while the host axis is off.
        -- `lib.order` reads it to refuse a move that would carry a group
        -- past a host boundary.
        host = group.host,
        target = session.id,
      }
      first = false
    end

    -- Placeholders at the end of the group, where the real row will appear.
    for _, item in ipairs(placeholders) do
      items[#items + 1] = {
        kind = "pending",
        command = item,
        -- A placeholder sits at group level, like the row it will become. Stated
        -- rather than left nil because every ordering helper compares `depth`
        -- numerically, and `nil` there is not a shallow row -- it is an error
        -- that takes the pane down on Shift+J/K while a session is being
        -- created. The `session` this row does NOT carry is the other half of
        -- the same contract: `lib.order`'s sort (see its `block_name`) holds a
        -- nameless block out of the comparison and puts it back at the group's
        -- end, so the placeholder stays here, where its session will appear.
        depth = 0,
        header = first and group_header or nil,
        host = group.host,
        target = false,
      }
      first = false
    end
  end

  model_cache.rows = all_rows
  model_cache.digest = digest
  model_cache.grouping = grouping
  model_cache.host_grouping = host_grouping
  model_cache.items = items
  return items
end

return session_model
