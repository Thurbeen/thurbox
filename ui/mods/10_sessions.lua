local fuzzy = require("lib.fuzzy")
local order = require("lib.order")
local panels = require("lib.panels")
local mod_settings = require("lib.settings")
local session_model = require("lib.session_model")
local theme = require("lib.theme")
local ui = require("lib.ui")
local widgets = require("lib.widgets")

local function current_action(session)
  local activity = session.activity
  activity = activity and activity:match("^%s*(.-)%s*$")
  if not activity or activity == "" then
    return nil
  end
  local stripped = activity:match("^✳%s*(.*)$") or activity:match("^%*%s*(.*)$") or activity
  if stripped == "" then
    return nil
  end
  return stripped
end

local function ago(since_ms)
  local now = thurbox.taken_at_ms
  if not since_ms or not now then
    return nil
  end
  local s = math.floor(math.max(0, now - since_ms) / 1000)
  if s < 60 then
    return s .. "s ago"
  elseif s < 3600 then
    return math.floor(s / 60) .. "m ago"
  end
  return math.floor(s / 3600) .. "h ago"
end

local function source_detail(session)
  local source = session.status_source
  if not source then
    return nil
  end
  if source == "none" then
    return "unknown · no hook, no screen rule"
  end
  local from = "hook (" .. (session.agent or "") .. ")"
  if source == "screen" then
    from = "screen (" .. (session.status_rule or "") .. ")"
  end
  local text = session.status .. " · " .. from
  local age = ago(session.status_since_ms)
  return age and (text .. " · " .. age) or text
end

local function agent_status(session)
  if session.deleting_since_ms then
    local text = "deletion stuck"
    local age = ago(session.deleting_since_ms)
    if age then
      text = text .. " · " .. age
    end
    if session.deleting_why then
      text = text .. " · " .. session.deleting_why
    end
    return text, nil
  end
  if session.status == "unreachable" then
    if session.fleet then
      return "Fleet " .. session.fleet .. " unreachable", nil
    end
    return "unreachable", nil
  end
  if session.state_why then
    return session.state_why, nil
  end
  local detail = source_detail(session)
  if detail then
    local said = current_action(session)
    if said then
      return nil, said .. " · " .. detail
    end
    return detail, nil
  end
  if session.status == "blocked" then
    return session.notification or "Blocked", nil
  end
  local action = current_action(session)
  local detected = session.detected_agent
  if detected then
    if action then
      return detected, action
    end
    return detected .. " · no status reported", nil
  end
  if action then
    return nil, action
  end
  if session.status == "uncovered" then
    return "no status hooks", nil
  end
  if session.status == "unreported" then
    return "no status reported", nil
  end
  if session.status == "running" then
    return "agent running · no status reported", nil
  end
  return nil, nil
end

local function search_query()
  local text = store["search.query"]
  if type(text) ~= "string" or text == "" then
    return nil
  end
  return text
end

local function name_hits(session, search)
  if not search then
    return nil
  end
  if search.matches and not search.matches[session.id] then
    return nil
  end
  local name = fuzzy.match_fields(search.q, { { name = "name", text = session.name or "" } })
  if name then
    return name.positions
  end
  if search.matches then
    return false
  end
  local fields = {
    { name = "name", text = session.name or "" },
    { name = "agent", text = session.agent or "" },
    { name = "branch", text = session.branch or "" },
    { name = "repo", text = session.repo or "" },
  }
  return fuzzy.match_fields(search.q, fields) and false or nil
end

local WIDE = 100

local function show_window_title()
  return mod_settings.enabled("sessions", "show_window_title", false)
end

local function title_of(session, terminal_title)
  if terminal_title then
    local action = current_action(session)
    if action then
      return action, true
    end
  end
  return session.name or "?", false
end

local function remote_mark(item)
  return item.host == nil and item.session.host ~= nil and item.depth == 0
end

local function folded_count(item)
  return item.collapsed and (" +" .. item.descendants) or nil
end

local function head_width(item, layout)
  local used = layout.base + 2 * item.depth + 4
  if item.cross_group then
    used = used + 2
  end
  if remote_mark(item) then
    used = used + 2
  end
  return used
end

local function action_column(items, layout)
  if layout.width + 2 < WIDE then
    return nil
  end
  local widest = 0
  for _, item in ipairs(items) do
    if item.kind == "session" then
      local title = title_of(item.session, layout.terminal_title)
      local suffix = folded_count(item)
      widest = math.max(
        widest,
        head_width(item, layout) + widgets.len(title) + (suffix and widgets.len(suffix) or 0)
      )
    end
  end
  return math.min(widest, math.floor(layout.width * 0.6)) + 2
end

local function session_line(item, layout, elapsed, is_selected, work, search)
  local session = item.session
  local spec = ui.status(session.status, elapsed, ui.printing(session.id), session.status_source)
  local glyph, glyph_color, glyph_dim = spec.glyph, spec.color, spec.dim
  local width = layout.width
  local reason, action = agent_status(session)
  local trailing_color = glyph_color
  if session.status ~= "blocked" then
    trailing_color = theme.muted
    local note = session.meta and session.meta["ui.note"]
    note = note and note:match("^%s*(.-)%s*$")
    if note and note ~= "" and session.status ~= "unreachable" then
      reason = reason and (note .. " · " .. reason) or note
    end
  end

  local title, from_terminal = title_of(session, layout.terminal_title)
  if from_terminal then
    action = nil
  end
  local trailing = reason
  if action then
    trailing = trailing and (trailing .. " · " .. action) or action
  end

  if work then
    if work.phase == "failed" then
      glyph, glyph_color, glyph_dim = "✗", theme.role("status_error"), nil
      trailing = work.error and ("failed: " .. work.error) or "failed"
      trailing_color = theme.role("status_error")
    else
      glyph, glyph_color, glyph_dim = "◌", theme.muted, nil
      trailing, trailing_color = work.kind, theme.muted
    end
  end

  local hits = name_hits(session, search)
  if from_terminal and hits then
    hits = false
  end

  local row = ui.row({
    width = width,
    tone = function(style)
      if is_selected then
        return style and style.dim and { dim = true } or nil
      end
      if search and hits == nil then
        return { fg = theme.muted }
      end
      return style
    end,
  })

  row:gap(layout.base + 2 * item.depth)
  if item.descendants > 0 then
    row:button(
      item.collapsed and "▸ " or "▾ ",
      { fg = theme.accent },
      nil,
      ":fold:" .. session.id
    )
  else
    row:gap(2)
  end
  row:add(glyph .. " ", { fg = glyph_color, dim = glyph_dim })
  if item.cross_group then
    row:add("↳ ", { fg = theme.muted })
  end
  if remote_mark(item) then
    row:add("@ ", { fg = theme.accent })
  end

  local title_style = { fg = theme.text, bold = item.descendants > 0 or nil }
  if session.status == "blocked" and not work then
    title_style = { fg = glyph_color, bold = true }
  end
  local suffix = folded_count(item)
  local suffix_width = suffix and widgets.len(suffix) or 0
  local title_end = (layout.column and trailing) and (layout.column - 2) or width
  title = widgets.truncate(title, math.max(1, title_end - row.used - suffix_width))

  row:match(title, hits, title_style, {
    fg = theme.accent,
    bold = true,
    underline = true,
  })
  if suffix then
    row:add(suffix, { fg = theme.muted })
  end

  if layout.column and trailing then
    row:gap(layout.column - row.used)
    return row
      :add(widgets.truncate_hard(trailing, width - row.used), { fg = trailing_color })
      :spans_list()
  end
  return row:trailing(trailing, { fg = trailing_color }):spans_list()
end

local PHASE_LABEL = {
  queued = "creating…",
  running = "creating…",
  resolving = "setting up…",
  hooks = "running hooks…",
  host = "on the host…",
  worktrees = "creating…",
  backend = "setting up…",
  launching = "spawning…",
  persisting = "spawning…",
}

local function pending_line(work, width, elapsed)
  local failed = work.phase == "failed"
  local glyph, glyph_style
  if failed then
    glyph, glyph_style = "✗", { fg = theme.role("status_error") }
  else
    glyph, glyph_style = theme.spinner_frame(elapsed), { fg = theme.warn }
  end

  local label = work.subject or "new session"
  local phase
  if failed then
    phase = work.error and ("failed: " .. work.error) or "failed"
  else
    phase = PHASE_LABEL[work.phase] or "creating…"
  end

  local row = ui.row({ width = width })
  row:add(" " .. glyph .. " ", glyph_style)
  row:add(label, { fg = theme.secondary })
  if width > row.used + widgets.len(phase) + 2 then
    row:add("  " .. phase, { fg = theme.muted })
  end
  return row:spans_list()
end

local function row_style(item)
  if item.kind == "host" then
    return { bg = theme.tint(theme.accent, 0.12) or theme.role("modal_bg") }
  end
  if item.kind == "session" and item.session.status == "blocked" then
    local bg = theme.tint(theme.role("status_blocked"), 0.16)
    return { bg = bg or theme.role("modal_bg") }
  end
  return nil
end

local function host_line(item, width, selected)
  local counts = item.counts
  local total = counts.total == 1 and "1 session" or counts.total .. " sessions"
  local details = total
  if counts.working > 0 then
    details = details .. " · " .. counts.working .. " active"
  end
  local local_host = item.host:sub(1, 1) == "\0"
  local wsl = not local_host and counts.backend and counts.backend:match("^wsl:") ~= nil
  for _, host in ipairs(thurbox.hosts or {}) do
    if host.name == item.host then
      wsl = host.backend:match("^wsl:") ~= nil
      break
    end
  end
  local color = theme.accent
  local reach = "connected"
  local reach_glyph = ""
  if counts.unreachable == counts.total and counts.total > 0 then
    reach, reach_glyph, color = "unreachable", "⊘", theme.role("status_unreachable")
  elseif counts.failed > 0 and counts.total == 0 then
    reach, reach_glyph, color = "failed", "✗", theme.role("status_error")
  elseif counts.total == 0 then
    reach, reach_glyph, color = "connecting", "◌", theme.warn
  end
  local row = ui.row({ width = width, tone = selected and function()
    return nil
  end or nil })
  row:add((item.collapsed and " ▸ " or " ▾ ") .. reach_glyph, { fg = color, bold = true })
  local label = local_host and "⌂ local" or wsl and ("@ WSL " .. item.host) or ("@ " .. item.host)
  row:add(label, { fg = theme.accent, bold = true })
  if counts.attention > 0 then
    row:add("  !" .. counts.attention, { fg = theme.role("status_blocked"), bold = true })
  end
  local note = reach == "connected" and details or reach .. " · " .. details
  local short = reach == "connected" and total or reach .. " · " .. total
  for _, candidate in ipairs({ note, short }) do
    local rule = width - row.used - widgets.len(candidate) - 2
    if rule >= 1 then
      row:add(" " .. string.rep("─", rule) .. " ", { fg = theme.muted })
      row:add(candidate, { fg = theme.secondary })
      return row:spans_list()
    end
  end
  row:trailing(short, { fg = theme.secondary })
  return row:spans_list()
end

local function repo_line(item, width, selected)
  local row = ui.row({ width = width, tone = selected and function()
    return nil
  end or nil })
  row:gap(item.host and 2 or 1)
  row:add(item.collapsed and "▸ " or "▾ ", { fg = theme.accent })
  local count = nil
  if item.collapsed then
    count = item.count == 1 and "1 session" or item.count .. " sessions"
  end
  local reserve = count and widgets.len(count) + 3 or 2
  row:add(widgets.truncate(item.repo_label, math.max(1, width - row.used - reserve)), {
    fg = theme.accent,
  })
  local rule = width - row.used - (count and widgets.len(count) + 2 or 1)
  if rule >= 1 then
    row:add(" " .. string.rep("╌", rule) .. (count and " " or ""), { fg = theme.muted })
  end
  if count then
    row:add(count, { fg = theme.secondary })
  end
  return row:spans_list()
end

local function sessions()
  return thurbox and thurbox.sessions or {}
end

local function focus_new_session()
  return mod_settings.enabled("sessions", "focus_new_session", false)
end

local function at_risk(session)
  local git = session.git
  if not git then
    return { "its state could not be read" }
  end

  local lines = {}
  local uncommitted = (git.files or 0) + (git.untracked or 0)
  if uncommitted > 0 then
    lines[#lines + 1] = uncommitted .. " uncommitted or untracked file(s)"
  elseif git.dirty then
    lines[#lines + 1] = "uncommitted changes"
  end
  if (git.ahead or 0) > 0 and git.merged ~= true then
    lines[#lines + 1] = git.ahead .. " commit(s) not pushed anywhere else"
  end

  local worktrees = session.worktrees or 0
  if worktrees > 1 then
    lines[#lines + 1] = "its other worktrees could not be read"
  end

  if #lines == 0 then
    return nil
  end

  if worktrees == 1 then
    lines[#lines + 1] = "its worktree directory"
  elseif worktrees > 1 then
    lines[#lines + 1] = "its " .. worktrees .. " worktree directories"
  end
  return lines
end

local function delete_for_good(session, question)
  local lines = at_risk(session)
  store.confirm = {
    question = question,
    lines = lines or {},
    command = "delete",
    options = { session = session.id, force = true },
  }
end

local function selected_session(item)
  return item and item.kind == "session" and item.target or nil
end

local function first_session_index(items)
  for index, item in ipairs(items) do
    if item.kind == "session" then
      return index
    end
  end
  return 1
end

local CURSOR_OPTS = {
  id = "target",
  steer = "selected",
  publish = selected_session,
  initial = first_session_index,
}
local CURSOR_OPTS_WITH_REQUEST = {
  id = "target",
  steer = "selected",
  publish = selected_session,
  initial = first_session_index,
  request = "focus_session",
}

local function folds(setting)
  local saved = mod_settings.get("sessions", setting, "")
  if type(saved) ~= "string" then
    saved = ""
  end
  if type(state[setting]) == "string" then
    if state[setting .. "_base"] == saved then
      return state[setting]
    end
    state[setting .. "_base"] = nil
    state[setting] = nil
  end
  return saved
end

local function folded_hosts()
  return folds("folded_hosts")
end

local function folded_repos()
  return folds("folded_repos")
end

local function escape_fold_key(name)
  return name:gsub("[^%w._-]", function(char)
    return string.format("%%%02X", string.byte(char))
  end)
end

local function is_folded(setting, name)
  local saved = ";" .. folds(setting) .. ";"
  return saved:find(";" .. escape_fold_key(name) .. ";", 1, true) ~= nil
end

local function save_folds(setting, value)
  state[setting .. "_base"] = mod_settings.get("sessions", setting, "")
  state[setting] = value
  command("set", { text = "sessions." .. setting, value = value })
end

local function set_folded(setting, name, folded)
  local names = {}
  for escaped in folds(setting):gmatch("[^;]+") do
    local decoded = escaped:gsub("%%(%x%x)", function(hex)
      return string.char(tonumber(hex, 16))
    end)
    names[decoded] = true
  end
  names[name] = folded or nil
  local ordered = {}
  for key in pairs(names) do
    ordered[#ordered + 1] = escape_fold_key(key)
  end
  table.sort(ordered)
  save_folds(setting, table.concat(ordered, ";"))
end

local function host_is_folded(host)
  return is_folded("folded_hosts", host)
end

local function set_host_folded(host, folded)
  set_folded("folded_hosts", host, folded)
end

local function repo_is_folded(target)
  return is_folded("folded_repos", target)
end

local function set_repo_folded(target, folded)
  set_folded("folded_repos", target, folded)
end

local function parent_is_folded(id)
  return is_folded("folded_parents", id)
end

local function set_parent_folded(id, folded)
  set_folded("folded_parents", id, folded)
end

local function folded_parents()
  return folds("folded_parents")
end

local function model_items(searching)
  return session_model.build(
    sessions(),
    folded_hosts(),
    searching,
    true,
    folded_repos(),
    folded_parents()
  )
end

---@param reveal_selected? boolean
local function model(reveal_selected)
  local has_sessions = #sessions() > 0
  if has_sessions and not state["sessions.had_session"] then
    state["sessions.cursor"] = nil
    state["sessions.selected"] = nil
  end
  state["sessions.had_session"] = has_sessions

  local searching = search_query() ~= nil
  local search_open = panels.shown("search") or searching
  local search_closed = state.search_was_open and not search_open
  state.search_was_open = search_open
  if search_closed then
    state["sessions.follow"] = nil
  end
  local accepted = store["search.accepted_session"]
  store["search.accepted_session"] = nil
  local wanted = accepted or store.focus_session
  local selected = store.selected
  if not wanted and not search_open and not search_closed then
    wanted = state["sessions.follow"]
    if not wanted and selected ~= state["sessions.published"] then
      wanted = selected
    end
    if not wanted and reveal_selected ~= false and selected then
      local items = model_items(searching)
      for _, item in ipairs(items) do
        if item.target == selected then
          return items
        end
      end
      wanted = selected
    end
  end
  if type(wanted) == "string" then
    local all = session_model.build(sessions(), "", false, true, "")
    for _, item in ipairs(all) do
      if item.kind == "session" and item.target == wanted and repo_is_folded(item.repo_target) then
        set_repo_folded(item.repo_target, false)
        break
      end
    end
    local parent_of, wanted_session = {}, nil
    for _, session in ipairs(sessions()) do
      parent_of[session.id] = session.parent
      if session.id == wanted then
        wanted_session = session
      end
    end
    if wanted_session then
      local host = wanted_session.host or "\0local"
      if host_is_folded(host) then
        set_host_folded(host, false)
      end
      if wanted_session.status ~= "blocked" then
        local ancestor, seen = parent_of[wanted], { [wanted] = true }
        while ancestor and not seen[ancestor] do
          seen[ancestor] = true
          if parent_is_folded(ancestor) then
            set_parent_folded(ancestor, false)
          end
          ancestor = parent_of[ancestor]
        end
      end
    end
  end
  return model_items(searching)
end

local function persist_order(items)
  local ids = {}
  for _, item in ipairs(items) do
    if item.kind == "session" and item.target then
      ids[#ids + 1] = item.target
    end
  end
  if #ids > 0 then
    command("order", { list = ids })
  end
end

local function ordering_items()
  local items = session_model.build(sessions(), "", false, true, "")
  local ordered = {}
  local previous_host
  for _, item in ipairs(items) do
    if item.kind ~= "host" and item.kind ~= "repo" then
      if item.host ~= previous_host and item.header == nil then
        local copy = {}
        for key, value in pairs(item) do
          copy[key] = value
        end
        copy.header = item.host or "local"
        ordered[#ordered + 1] = copy
      else
        ordered[#ordered + 1] = item
      end
      previous_host = item.host
    end
  end
  return ordered
end

local RUNNING = { working = true, blocked = true, idle = true, running = true }

local function resume_hint()
  local chord = ui.chord("sessions.start")
  if not chord then
    return "Start in its menu resumes it on its conversation."
  end
  return chord .. " resumes it on its conversation."
end

local SESSION_MENU = {
  { label = "Open", action = "sessions.open" },
  { label = "Rename", action = "sessions.rename" },
  { label = "Open in editor", action = "sessions.editor" },
  { label = "Start", action = "sessions.start" },
  { label = "Pause", action = "sessions.pause" },
  { label = "Restart", action = "sessions.restart" },
  { label = "Restart fresh", action = "sessions.restart_fresh" },
  "sep",
  { label = "Move up", action = "sessions.move_up" },
  { label = "Move down", action = "sessions.move_down" },
  "sep",
  { label = "Delete", action = "sessions.delete" },
}

local function declared(action)
  local registry = thurbox.registry
  for _, list in ipairs({ registry and registry.keys, registry and registry.commands }) do
    for _, row in ipairs(list or {}) do
      if row.action == action then
        return true
      end
    end
  end
  return false
end

local function fold_menu_entries()
  local expanded, collapsed = false, false
  for _, item in ipairs(session_model.build(sessions(), "", false, true, "")) do
    local parent = item.kind == "session" and item.descendants > 0
    if item.kind == "host" or item.kind == "repo" or parent then
      local folded = item.kind == "host" and host_is_folded(item.host)
        or item.kind == "repo" and repo_is_folded(item.target)
        or parent and parent_is_folded(item.target)
      collapsed = collapsed or folded
      expanded = expanded or not folded
    end
  end
  local entries = {}
  if expanded then
    entries[#entries + 1] = { label = "Collapse all", action = "sessions.collapse_all" }
  end
  if collapsed then
    entries[#entries + 1] = { label = "Expand all", action = "sessions.expand_all" }
  end
  return entries
end

local function row_menu()
  ---@type (table|string)[]
  local base = {}
  for _, entry in ipairs(SESSION_MENU) do
    base[#base + 1] = entry
  end
  local fold_entries = fold_menu_entries()
  if #fold_entries > 0 then
    base[#base + 1] = "sep"
    for _, entry in ipairs(fold_entries) do
      base[#base + 1] = entry
    end
  end
  local extra = store["sessions.menu_extra"]
  if type(extra) ~= "table" then
    return base
  end
  local owners = {}
  for owner, entries in pairs(extra) do
    if type(owner) == "string" and type(entries) == "table" then
      owners[#owners + 1] = owner
    end
  end
  if #owners == 0 then
    return base
  end
  table.sort(owners)
  ---@type (table|string)[]
  local menu = {}
  for i, entry in ipairs(base) do
    menu[i] = entry
  end
  for _, owner in ipairs(owners) do
    local rule = true
    for _, entry in ipairs(extra[owner]) do
      if entry == "sep" then
        rule = true
      elseif
        type(entry) == "table"
        and type(entry.action) == "string"
        and declared(entry.action)
      then
        if rule then
          menu[#menu + 1] = "sep"
          rule = false
        end
        local label = type(entry.label) == "string" and entry.label or nil
        menu[#menu + 1] = { label = label, action = entry.action }
      end
    end
  end
  return menu
end

local function pane_menu(items)
  ---@type (table|string)[]
  local menu = {
    { label = "New session", action = "new_session.open" },
  }
  local live = false
  for _, item in ipairs(items) do
    if item.session then
      live = true
      break
    end
  end
  local middle = {}
  if live then
    middle[#middle + 1] = { label = "Sort by name", action = "sessions.sort" }
  end
  for _, entry in ipairs(fold_menu_entries()) do
    middle[#middle + 1] = entry
  end
  if #middle > 0 then
    menu[#menu + 1] = "sep"
    for _, entry in ipairs(middle) do
      menu[#menu + 1] = entry
    end
  end
  menu[#menu + 1] = "sep"
  menu[#menu + 1] = { label = "Hide panel", action = "sessions.toggle_panel" }
  return menu
end

local pane
pane = {
  name = "sessions",
  slot = "sessions",
  order = 10,
  focusable = true,
  pure = true,

  ui_state = function()
    local _, folded_count = folded_hosts():gsub("[^;]+", "")
    local selected
    for _, item in ipairs(session_model.build(sessions(), "", false, true, "")) do
      if item.target == state["sessions.selected"] then
        selected = item
        break
      end
    end
    local host = selected and selected.host
    local repo = selected and (selected.kind == "repo" and selected.target or selected.repo_target)
    return {
      selected_row = state["sessions.selected"],
      selected_host = host == "\0local" and "local" or host,
      host_is_local = host == "\0local",
      host_collapsed = host and host_is_folded(host) or false,
      folded_host_count = folded_count,
      repo_collapsed = repo and repo_is_folded(repo) or false,
    }
  end,

  actions = {
    {
      name = "sessions.collapse_host",
      desc = "fold group",
      args = { { name = "host", kind = "string" } },
    },
    {
      name = "sessions.expand_host",
      desc = "unfold group",
      args = { { name = "host", kind = "string" } },
    },
    {
      name = "sessions.toggle_host",
      desc = "toggle host or repo fold",
      args = { { name = "host", kind = "string" } },
    },
  },

  settings = {
    {
      id = "group_by_repo",
      desc = "Group sessions under foldable repo rows",
      default = true,
    },
    {
      id = "group_by_host",
      desc = "Group sessions under foldable host rows",
      default = true,
    },
    {
      id = "show_window_title",
      desc = "Show the agent's terminal title instead of the session name",
      default = false,
    },
    {
      id = "focus_new_session",
      desc = "Select and open a session when you create it",
      default = false,
    },
    {
      id = "folded_hosts",
      desc = "Folded hosts (managed by the session list)",
      default = "",
      list = true,
    },
    {
      id = "folded_repos",
      desc = "Folded repos (managed by the session list)",
      default = "",
      list = true,
    },
    {
      id = "folded_parents",
      desc = "Folded parent sessions (managed by the session list)",
      default = "",
      list = true,
    },
  },

  events = { "session.post_create" },

  keys = {
    { key = "j", action = "sessions.next", desc = "next session", group = "Navigation" },
    { key = "k", action = "sessions.previous", desc = "previous session", group = "Navigation" },
    { key = "down", action = "sessions.next", desc = "next session", group = "Navigation" },
    { key = "up", action = "sessions.previous", desc = "previous session", group = "Navigation" },
    { key = "g", action = "sessions.first", desc = "first session", group = "Navigation" },
    { key = "home", action = "sessions.first", desc = "first row", group = "Navigation" },
    { key = "end", action = "sessions.last", desc = "last row", group = "Navigation" },
    { key = "G", action = "sessions.last", desc = "last row", group = "Navigation" },
    { key = "pageup", action = "sessions.page_up", desc = "previous page", group = "Navigation" },
    { key = "pagedown", action = "sessions.page_down", desc = "next page", group = "Navigation" },
    { key = "[", action = "sessions.previous_host", desc = "previous host", group = "Navigation" },
    { key = "]", action = "sessions.next_host", desc = "next host", group = "Navigation" },
    {
      key = "n",
      action = "sessions.next_attention",
      desc = "next session needing attention",
      group = "Navigation",
    },
    { key = "H", action = "sessions.collapse_all", desc = "fold all groups", group = "Navigation" },
    { key = "L", action = "sessions.expand_all", desc = "unfold all groups", group = "Navigation" },
    { key = "h", action = "sessions.collapse_host", desc = "fold group", group = "Navigation" },
    { key = "l", action = "sessions.expand_host", desc = "unfold group", group = "Navigation" },
    {
      key = "left",
      action = "sessions.parent_host",
      desc = "fold group or select its parent",
      group = "Navigation",
    },
    {
      key = "right",
      action = "sessions.first_child",
      desc = "unfold group or select its first child",
      group = "Navigation",
    },
    { key = "enter", action = "sessions.open", desc = "open the session", group = "Navigation" },
    { key = "J", action = "sessions.move_down", desc = "move session down", group = "Sessions" },
    { key = "K", action = "sessions.move_up", desc = "move session up", group = "Sessions" },
    { key = "S", action = "sessions.sort", desc = "sort sessions by name", group = "Sessions" },
    { key = "s", action = "sessions.start", desc = "start a stopped session", group = "Sessions" },
    { key = "p", action = "sessions.pause", desc = "pause a running session", group = "Sessions" },
    {
      key = "R",
      action = "sessions.restart",
      desc = "restart a session on its conversation",
      group = "Sessions",
    },

    {
      key = "ctrl+d",
      action = "sessions.delete",
      desc = "delete session",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+o",
      action = "sessions.editor",
      desc = "open the session's directory in your editor",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+e",
      action = "sessions.rename",
      desc = "rename session",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+j",
      action = "sessions.next",
      desc = "next session",
      scope = "global",
      group = "Navigation",
    },
    {
      key = "ctrl+k",
      action = "sessions.previous",
      desc = "previous session",
      scope = "global",
      group = "Navigation",
    },
    {
      key = "f9",
      action = "sessions.toggle_panel",
      desc = "toggle the session list",
      scope = "global",
      group = "Panels",
    },
  },

  commands = {
    { action = "sessions.restart_fresh", desc = "restart a session on a new conversation" },
  },

  render = function(ctx)
    local width = math.max(0, ctx.width or 0)
    local height = math.max(0, ctx.height or 0)
    if width < 2 or height < 2 then
      return { type = "text", text = "" }
    end
    local inner_width = width - 2

    local items = model()
    local busy = session_model.pending()
    local query = search_query()
    local search = nil
    if query then
      local matches = nil
      if type(store["search.matches"]) == "string" then
        matches = {}
        for id in store["search.matches"]:gmatch("%S+") do
          matches[id] = true
        end
      end
      search = { q = fuzzy.query(query), matches = matches }
    end

    local cursor = ui.cursor("sessions", items, CURSOR_OPTS_WITH_REQUEST)
    state["sessions.page_size"] = math.max(1, height - 2)

    local base = 1
    for _, item in ipairs(items) do
      if item.kind == "repo" then
        base = 3
        break
      elseif item.kind == "host" then
        base = 2
      end
    end
    local layout = {
      width = inner_width,
      base = base,
      terminal_title = show_window_title(),
    }
    layout.column = action_column(items, layout)

    return ui.panel({
      title = "Sessions",
      focused = ctx.focused,
      body = ui.list({
        items = items,
        cursor = cursor,
        width = inner_width,
        height = height - 2,
        on_overflow = "border",
        pad = true,
        style_of = row_style,
        class_of = function(item)
          if item.kind == "host" then
            return "host-row"
          end
          if item.kind == "repo" then
            return "repo-row"
          end
          return item.kind == "pending" and "pending-row" or "session-row"
        end,
        row = function(item, selected)
          if item.kind == "host" then
            return host_line(item, inner_width, selected)
          end
          if item.kind == "repo" then
            return repo_line(item, inner_width, selected)
          end
          if item.kind == "pending" then
            return pending_line(item.command, inner_width, ctx.elapsed)
          end
          return session_line(item, layout, ctx.elapsed, selected, busy[item.session.id], search)
        end,
        empty = ui.empty({
          title = "No sessions yet",
          width = inner_width,
          hint = "Press %s to create one",
          hint_action = "new_session.open",
        }),
      }),
    })
  end,

  on_event = function(name, payload)
    if name ~= "session.post_create" or not focus_new_session() then
      return
    end
    local id = payload.session
    if not id then
      return
    end
    ui.follow("sessions", id)
    store.selected = id
    command("focus", { text = "agent" })
  end,

  on_click = function(hit)
    if not hit.id then
      return false
    end
    local parent = hit.id:match("^:fold:(.+)$")
    if parent then
      if hit.clicks ~= 2 then
        local cursor = ui.cursor("sessions", model(), CURSOR_OPTS)
        local selected = cursor:id()
        cursor:select(cursor.index)
        set_parent_folded(parent, not parent_is_folded(parent))
        for _, item in ipairs(model(false)) do
          if item.target == selected then
            return true
          end
        end
        cursor:select_by_id(parent)
      end
      return true
    end
    local items = model()
    if hit.id:match("^:host:") or hit.id:match("^:repo:") then
      if ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) == nil then
        return false
      end
      if hit.clicks ~= 2 then
        pane.on_action("sessions.toggle_host")
      end
      return true
    end
    if ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) == nil then
      return false
    end
    if hit.clicks == 2 then
      command("focus", { text = "agent" })
    end
    return true
  end,

  on_context = function(hit)
    local items = model()
    local parent = hit.id and hit.id:match("^:fold:(.+)$")
    if parent then
      hit = { id = parent, screen_x = hit.screen_x, screen_y = hit.screen_y }
    end
    if hit.id and (hit.id:match("^:host:") or hit.id:match("^:repo:")) then
      if ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) == nil then
        return false
      end
      return pane.on_action("sessions.toggle_host")
    end
    if not hit.id then
      store.menu = { at = { x = hit.screen_x, y = hit.screen_y }, items = pane_menu(items) }
      return true
    end
    if ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) == nil then
      return false
    end
    store.menu = {
      at = { x = hit.screen_x, y = hit.screen_y },
      items = row_menu(),
      target = hit.id,
      target_argument = "session_id",
    }
    return true
  end,

  on_action = function(action, args)
    if action == "sessions.toggle_panel" then
      panels.toggle("sessions")
      return true
    end
    local items = model()
    if #items == 0 then
      return false
    end
    local cursor = ui.cursor("sessions", items, CURSOR_OPTS)

    local requested_host = type(args) == "table" and args.host or nil
    if requested_host then
      local host = requested_host == "" and "\0local" or requested_host
      if cursor:select_by_id(":host:" .. host) == nil then
        command("message", { text = "that host has no group", level = "error" })
        return true
      end
    end

    local target = type(args) == "table" and args.session_id or nil
    local chosen = store["menu.chosen"]
    if type(chosen) == "table" and chosen.action == action then
      store["menu.chosen"] = nil
      target = target or chosen.target
    end
    if target then
      if cursor:select_by_id(target) == nil then
        command("message", { text = "that session is gone", level = "error" })
        return true
      end
    end

    local at = cursor.index
    local id = cursor:id()
    local selected = items[at]

    local group = selected and (selected.kind == "host" or selected.kind == "repo")
    local parent = selected and selected.kind == "session" and selected.descendants > 0
    if action == "sessions.parent_host" then
      if group and not selected.collapsed then
        return pane.on_action("sessions.collapse_host")
      elseif parent and not selected.collapsed then
        set_parent_folded(selected.target, true)
      elseif
        selected
        and selected.kind == "session"
        and selected.depth > 0
        and cursor:select_by_id(selected.visible_parent)
      then
        return true
      elseif selected and selected.kind ~= "host" then
        local repo = selected.kind ~= "repo" and selected.repo_target
        if not (repo and cursor:select_by_id(repo)) and selected.host then
          cursor:select_by_id(":host:" .. selected.host)
        end
      end
      return true
    elseif action == "sessions.first_child" then
      if parent then
        if selected.collapsed then
          set_parent_folded(selected.target, false)
        elseif items[at + 1] and items[at + 1].depth == selected.depth + 1 then
          cursor:select(at + 1)
        end
        return true
      end
      if not group then
        return true
      end
      if selected.collapsed then
        return pane.on_action("sessions.expand_host")
      end
      local child = items[at + 1]
      if
        child
        and (
          selected.kind == "host" and child.host == selected.host
          or child.repo_target == selected.target
        )
      then
        cursor:select(at + 1)
      end
      return true
    end

    if action == "sessions.toggle_host" then
      if group then
        return pane.on_action(
          selected.collapsed and "sessions.expand_host" or "sessions.collapse_host"
        )
      end
      return false
    end

    if action == "sessions.collapse_all" or action == "sessions.expand_all" then
      local folding = action == "sessions.collapse_all"
      local hosts, repos, parents = {}, {}, {}
      local all = session_model.build(sessions(), "", false, true, "")
      if folding then
        for _, item in ipairs(all) do
          if item.kind == "host" then
            hosts[#hosts + 1] = escape_fold_key(item.host)
          elseif item.kind == "repo" then
            repos[#repos + 1] = escape_fold_key(item.target)
          elseif item.kind == "session" and item.descendants > 0 then
            parents[#parents + 1] = escape_fold_key(item.target)
          end
        end
        cursor:select(1)
      end
      table.sort(hosts)
      table.sort(repos)
      table.sort(parents)
      save_folds("folded_hosts", table.concat(hosts, ";"))
      save_folds("folded_repos", table.concat(repos, ";"))
      save_folds("folded_parents", table.concat(parents, ";"))
      return true
    end

    if action == "sessions.next_host" or action == "sessions.previous_host" then
      local step = action == "sessions.next_host" and 1 or -1
      for offset = 1, #items do
        local index = (at - 1 + offset * step) % #items + 1
        if items[index].kind == "host" then
          cursor:select(index)
          break
        end
      end
      return true
    end

    if action == "sessions.next_attention" then
      local all = session_model.build(sessions(), "", false, true, "")
      local start = 0
      for index, item in ipairs(all) do
        if item.target == cursor:id() then
          start = index
          break
        end
      end
      for offset = 1, #all do
        local item = all[(start + offset - 1) % #all + 1]
        local status = item.session and item.session.status
        if
          status == "blocked"
          or status == "done"
          or status == "error"
          or status == "unreachable"
        then
          cursor:follow(item.target)
          store.selected = item.target
          return true
        end
      end
      command("message", { text = "no sessions need attention" })
      return true
    end

    if action == "sessions.collapse_host" or action == "sessions.expand_host" then
      local folding = action == "sessions.collapse_host"
      if selected and selected.kind == "repo" then
        set_repo_folded(selected.target, folding)
      else
        local host = selected and selected.host
        if host then
          if folding then
            cursor:select_by_id(":host:" .. host)
          end
          set_host_folded(host, folding)
        end
      end
      return true
    end
    if selected and selected.kind ~= "session" then
      id = nil
    end

    if action == "sessions.open" then
      if id then
        command("focus", { text = "agent" })
      elseif group then
        return pane.on_action("sessions.toggle_host")
      end
    elseif action == "sessions.next" then
      cursor:move(1)
    elseif action == "sessions.previous" then
      cursor:move(-1)
    elseif action == "sessions.first" then
      cursor:select(1)
    elseif action == "sessions.last" then
      cursor:select(#items)
    elseif action == "sessions.page_up" or action == "sessions.page_down" then
      local step = action == "sessions.page_down" and 1 or -1
      cursor:select(math.max(1, math.min(#items, at + step * (state["sessions.page_size"] or 1))))
    elseif action == "sessions.delete" and id then
      local session = items[at].session
      delete_for_good(session, "Delete " .. (session.name or "this session") .. " for good?")
    elseif action == "sessions.rename" and id then
      store.rename = { session = id, name = items[at].session.name or "" }
    elseif action == "sessions.start" and id then
      local session = items[at].session
      local revival = session_model.revival(session)
      if session.starting then
        command("message", { text = (session.name or "this session") .. " is already starting" })
      elseif revival == "start" then
        command("start", { session = id })
      elseif revival == "recreate" then
        command("message", { text = (session.name or "this session") .. " cannot resume" })
      else
        command("message", { text = (session.name or "this session") .. " is not stopped" })
      end
    elseif action == "sessions.pause" and id then
      local session = items[at].session
      local name = session.name or "this session"
      if session.starting then
        command("message", { text = name .. " is starting" })
      elseif session.status == "stopped" then
        command("message", { text = name .. " is already paused" })
      elseif RUNNING[session.status] and not session_model.revival(session) then
        store.confirm = {
          question = "Pause " .. name .. "?",
          lines = { "Its agent stops. " .. resume_hint() },
          command = "pause",
          options = { session = id },
        }
      else
        command("message", { text = name .. " is not running" })
      end
    elseif (action == "sessions.restart" or action == "sessions.restart_fresh") and id then
      local session = items[at].session
      local name = session.name or "this session"
      local fresh = action == "sessions.restart_fresh"
      if session.starting then
        command("message", { text = name .. " is starting" })
      elseif
        session.status == "stopped"
        or (RUNNING[session.status] and not session_model.revival(session))
      then
        store.confirm = {
          question = "Restart " .. name .. "?",
          lines = {
            fresh and "Its agent stops and starts on a new conversation."
              or "Its agent stops and resumes its conversation if Fleet can.",
          },
          command = "restart",
          options = { session = id, fresh = fresh },
        }
      else
        command("message", { text = name .. " has no agent to restart" })
      end
    elseif action == "sessions.editor" and id then
      command("editor", { session = id })
    elseif action == "sessions.move_down" and id then
      local arranged = ordering_items()
      local position
      for index, item in ipairs(arranged) do
        if item.target == id then
          position = index
          break
        end
      end
      local moved = position and order.move_block(arranged, position, true)
      if moved then
        cursor:follow(id)
        persist_order(moved)
      end
    elseif action == "sessions.move_up" and id then
      local arranged = ordering_items()
      local position
      for index, item in ipairs(arranged) do
        if item.target == id then
          position = index
          break
        end
      end
      local moved = position and order.move_block(arranged, position, false)
      if moved then
        cursor:follow(id)
        persist_order(moved)
      end
    elseif action == "sessions.sort" then
      cursor:follow(id)
      persist_order(order.sorted_within_groups(ordering_items()))
    else
      return false
    end
    return true
  end,
}

return pane
