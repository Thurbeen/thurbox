local chrome = require("lib.chrome")
local fuzzy = require("lib.fuzzy")
local hover = require("lib.hover")
local panels = require("lib.panels")
local textinput = require("lib.textinput")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

local NAME = "search"
local OPEN = "search.open"
local NEXT, PREVIOUS = "search.next", "search.previous"
local PAGE_DOWN, PAGE_UP = "search.page_down", "search.page_up"
local ACTIVATE, CANCEL = "search.activate", "search.cancel"
local SCOPE = "search.scope"

local QUERY = "search.query"

local MATCHES = "search.matches"

local WANT_CONTENT = "want_content"
local WANT_SESSIONS = "want_content.sessions"

local REVEAL = "terminal.reveal"

local PAGE = 10

local SCOPES = { "all", "text", "names" }
local SCOPE_LABEL = { all = "everything", text = "terminal text", names = "names" }

local SECTION = { sessions = "sessions", text = "text" }

local function load()
  return {
    field = state.field or textinput.new(""),
    cursor = state.cursor or 1,
    snapshot = state.snapshot,
    previewed = state.previewed,
    revealed = state.revealed,
    scope = state.scope or "all",
  }
end

local function save(search)
  state.field = search.field
  state.cursor = search.cursor
  state.snapshot = search.snapshot
  state.previewed = search.previewed
  state.revealed = search.revealed
  state.scope = search.scope
end

local function query()
  return store[QUERY] or ""
end

local function fields_of(session)
  return {
    { name = "name", text = session.name or "" },
    { name = "agent", text = session.agent or "" },
    { name = "branch", text = session.branch or "" },
    { name = "repo", text = session.repo or "" },
  }
end

local function allowed(q)
  local list = {}
  for _, session in ipairs(thurbox.sessions or {}) do
    if fuzzy.passes(q, session) then
      list[#list + 1] = session
    end
  end
  return list
end

local function ask_of(q, scope, sessions)
  if scope == "names" or q.empty then
    return nil
  end
  local within = nil
  if next(q.filters) then
    local ids = {}
    for _, session in ipairs(sessions) do
      ids[#ids + 1] = session.id
    end
    within = table.concat(ids, " ")
  end
  return { terms = q.terms, within = within }
end

local function answer_for(ask)
  local answer = thurbox and thurbox.search
  if not ask or not answer then
    return nil
  end
  if answer.query ~= ask.terms or answer.within ~= ask.within then
    return nil
  end
  return answer
end

local function compute(text, scope)
  local q = fuzzy.query(text)
  local sessions = allowed(q)
  local rows = {}

  if scope ~= "text" then
    local found = {}
    for order, session in ipairs(sessions) do
      local m = q.empty and { score = 0, positions = {} }
        or fuzzy.match_fields(q, fields_of(session))
      if m then
        found[#found + 1] = {
          scope = "sessions",
          id = session.id,
          session = session.id,
          label = session.name or session.id,
          positions = m.positions,
          snippet = m.field and (m.field .. ": " .. m.text) or nil,
          score = m.score,
          order = order,
        }
      end
    end
    table.sort(found, function(a, b)
      if a.score ~= b.score then
        return a.score > b.score
      end
      return a.order < b.order
    end)
    for _, row in ipairs(found) do
      rows[#rows + 1] = row
    end
  end

  local ask = ask_of(q, scope, sessions)
  local answer = answer_for(ask)
  if answer then
    local names = {}
    for _, session in ipairs(sessions) do
      names[session.id] = session.name or session.id
    end
    for index, hit in ipairs(answer.hits or {}) do
      if names[hit.session] then
        rows[#rows + 1] = {
          scope = "text",
          id = "hit:" .. index,
          session = hit.session,
          label = names[hit.session],
          hit = hit,
        }
      end
    end
  end

  return rows,
    {
      q = q,
      sessions = #sessions,
      ask = ask,
      answer = answer,
      searching = ask ~= nil and answer == nil,
    }
end

local memo = {}

local function results(scope)
  local text = query()
  local sessions_table, answer_table = thurbox.sessions, thurbox and thurbox.search
  if
    memo.rows
    and memo.text == text
    and memo.scope == scope
    and memo.sessions == sessions_table
    and memo.answer == answer_table
  then
    return memo.rows, memo.info
  end
  local rows, info = compute(text, scope)
  memo = {
    text = text,
    scope = scope,
    sessions = sessions_table,
    answer = answer_table,
    rows = rows,
    info = info,
  }
  return rows, info
end

local function counts(rows)
  local totals = {}
  for _, row in ipairs(rows) do
    totals[row.scope] = (totals[row.scope] or 0) + 1
  end
  return totals
end

local function matched_ids(rows)
  local seen, ids = {}, {}
  for _, row in ipairs(rows) do
    if not seen[row.session] then
      seen[row.session] = true
      ids[#ids + 1] = row.session
    end
  end
  return table.concat(ids, " ")
end

local function surface_of(hit)
  return hit.session .. (hit.shell and "#shell" or "")
end

local function reveal(requests)
  if #requests == 0 then
    return
  end
  store[REVEAL] = table.concat(requests, ";")
  command("action", { text = REVEAL })
end

local function place_of(row)
  if not row then
    return nil
  end
  local hit = row.hit
  if not hit then
    return row.id
  end
  return row.id
    .. " "
    .. surface_of(hit)
    .. " "
    .. math.floor(hit.scroll or 0)
    .. " "
    .. math.floor(hit.row or 0)
end

local function preview(search, row, scroll)
  if not row then
    return
  end
  panels.show("sessions")
  store.selected = row.session
  if not scroll then
    return
  end
  local requests = {}
  local surface = row.hit and surface_of(row.hit) or nil
  if search.revealed and search.revealed ~= surface then
    requests[#requests + 1] = "-" .. search.revealed
  end
  if row.hit then
    requests[#requests + 1] = surface
      .. " "
      .. math.floor(row.hit.scroll or 0)
      .. " "
      .. math.floor(row.hit.row or 0)
  end
  search.revealed = surface
  reveal(requests)
end

local function restore(search)
  if search.revealed then
    reveal({ "-" .. search.revealed })
  end
  local snapshot = search.snapshot
  if not snapshot then
    return
  end
  if snapshot.selected then
    store.selected = snapshot.selected
  end
  if snapshot.sessions_shown ~= nil then
    store["panels.sessions"] = snapshot.sessions_shown
  end
end

local function want_content(ask, scope)
  if not ask then
    -- MUST KILL: rename — a `WARM_SCROLLBACK_CACHE` constant would say the empty query asks the kernel to pre-read every terminal
    store[WANT_CONTENT] = scope ~= "names" and "" or nil
    store[WANT_SESSIONS] = nil
    return
  end
  store[WANT_CONTENT] = ask.terms
  store[WANT_SESSIONS] = ask.within
end

local function close(search, keep)
  if not keep then
    restore(search)
  end
  search.snapshot = nil
  textinput.clear(search.field)
  search.cursor = 1
  search.previewed = nil
  search.revealed = nil
  save(search)
  store[QUERY] = nil
  store[MATCHES] = nil
  store[WANT_CONTENT] = nil
  store[WANT_SESSIONS] = nil
  panels.hide(NAME)
end

local function activate(search, row)
  preview(search, row, true)
  store["search.accepted_session"] = row and row.session or nil
  close(search, true)
  command("focus", { text = "agent" })
end

local function thousands(n)
  local text = tostring(math.floor(n or 0))
  local out = text:reverse():gsub("(%d%d%d)", "%1,"):reverse()
  return (out:gsub("^,", ""))
end

local function query_row(search, rows, info)
  local total = #rows
  local position = total > 0 and math.min(search.cursor, total) or 0
  local tail
  if total > 0 then
    tail = "[" .. position .. "/" .. total .. "]"
  elseif info.searching then
    tail = "searching…"
  else
    tail = "no matches"
  end
  local label = search.scope == "all" and " Search " or (" Search " .. search.scope .. " ")
  return {
    type = "box",
    axis = "horizontal",
    len = 1,
    children = {
      {
        type = "text",
        len = widgets.len(label),
        text = { { { text = label, style = { fg = theme.accent, bold = true } } } },
      },
      {
        type = "input",
        fill = 1,
        value = search.field.value or "",
        cursor = search.field.cursor or 0,
        placeholder = 'words, "a phrase", /regex/, in:session, repo:name',
        focused = true,
        style = { fg = theme.text },
      },
      {
        type = "text",
        len = widgets.len(tail) + 1,
        text = { { { text = tail .. " ", style = { fg = theme.muted } } } },
      },
    },
  }
end

local function status_line(search, rows, info)
  local parts = {}
  local totals = counts(rows)
  if search.scope ~= "text" then
    parts[#parts + 1] = "names " .. (totals.sessions or 0)
  end
  if search.scope ~= "names" then
    local answer = info.answer
    if info.q.empty then
      parts[#parts + 1] = "type to search terminal text"
    elseif info.searching then
      parts[#parts + 1] = "searching " .. info.sessions .. " sessions' scrollback…"
    elseif answer and answer.error then
      parts[#parts + 1] = answer.error
    elseif answer then
      local shown = totals.text or 0
      local found = answer.total or shown
      parts[#parts + 1] = "text "
        .. shown
        .. (found > shown and (" of " .. thousands(found)) or "")
        .. " in "
        .. thousands(answer.lines)
        .. " lines of "
        .. answer.sessions
        .. " sessions ("
        .. string.format((answer.ms or 0) < 10 and "%.1f" or "%.0f", answer.ms or 0)
        .. "ms)"
    end
  end
  parts[#parts + 1] = "tab: " .. SCOPE_LABEL[search.scope]
  return {
    type = "text",
    len = 1,
    text = { { { text = " " .. table.concat(parts, " · "), style = { fg = theme.muted } } } },
  }
end

local function session_line(row, selected, width)
  local base = { fg = selected and theme.text or theme.secondary }
  local hit = { fg = theme.accent, bold = true, underline = true }
  local spans = fuzzy.spans(row.label, row.positions, base, hit)
  if row.snippet then
    local used = widgets.len(row.label) + 2
    local room = math.max(0, width - used - 3)
    if room > 4 then
      spans[#spans + 1] = {
        text = "  " .. widgets.truncate(row.snippet, room),
        style = { fg = theme.muted },
      }
    end
  end
  return { spans = spans, id = row.id }
end

local function position_of(hit)
  local back = hit.back or 0
  if (hit.scroll or 0) == 0 then
    return "on screen"
  end
  return back .. "↑"
end

local function text_line(row, selected, width)
  local hit = row.hit
  local base = { fg = selected and theme.text or theme.secondary }
  local lit = { fg = theme.accent, bold = true, underline = true }
  local spans = {}
  if width >= 40 then
    local name = row.label .. (hit.shell and " ·sh" or "")
    local column = math.min(18, math.max(8, math.floor(width / 5)))
    spans[#spans + 1] = {
      text = widgets.pad(widgets.truncate(name, column), column) .. " ",
      style = { fg = theme.muted },
    }
  end
  local where = position_of(hit)
  spans[#spans + 1] = {
    text = string.rep(" ", math.max(0, 9 - widgets.len(where))) .. where .. "  ",
    style = { fg = theme.muted },
  }
  for _, span in ipairs(fuzzy.spans(hit.text or "", hit.positions, base, lit)) do
    spans[#spans + 1] = span
  end
  return { spans = spans, id = row.id }
end

local function result_rows(rows, cursor, width, height, info, scope)
  local entries, selected_line = {}, 1
  local totals = counts(rows)
  local section = nil
  for index, row in ipairs(rows) do
    if row.scope ~= section then
      section = row.scope
      entries[#entries + 1] = { header = SECTION[section] .. " " .. totals[section], key = section }
    end
    entries[#entries + 1] = { row = row, index = index }
    if index == cursor then
      selected_line = #entries
    end
  end
  if info.searching and scope ~= "names" and #rows > 0 then
    entries[#entries + 1] = { header = "text: searching…", key = "searching" }
  end

  local first, last = widgets.window(#entries, height, selected_line)
  local lines = {}
  for at, entry in ipairs(entries) do
    if at < first or at > last then
      lines[at] = { spans = {}, id = entry.header and ("header:" .. entry.key) or entry.row.id }
    elseif entry.header then
      lines[at] = {
        spans = { { text = entry.header, style = { fg = theme.muted } } },
        id = "header:" .. entry.key,
        hover = false,
      }
    elseif entry.row.hit then
      lines[at] = text_line(entry.row, entry.index == cursor, width)
    else
      lines[at] = session_line(entry.row, entry.index == cursor, width)
    end
  end
  return lines, selected_line
end

local function empty_text(search, info)
  if info.sessions == 0 then
    return "  no session passes the filter"
  end
  if info.searching then
    return "  searching " .. info.sessions .. " sessions…"
  end
  local what = search.scope == "names" and "names"
    or search.scope == "text" and "terminal text"
    or "names or terminal text"
  local lines = info.answer and (" (" .. thousands(info.answer.lines) .. " lines)") or ""
  return "  no match for "
    .. info.q.raw
    .. " in the "
    .. what
    .. " of "
    .. info.sessions
    .. " sessions"
    .. lines
end

local function step_cursor(step)
  local search = load()
  local rows = results(search.scope)
  search.cursor = widgets.clamp(search.cursor + step, #rows)
  local row = rows[search.cursor]
  search.previewed = place_of(row)
  preview(search, row, true)
  save(search)
end

return {
  name = NAME,
  ui_state = function()
    if store[QUERY] == nil then
      return { open = false }
    end
    return { open = true, selected_result = state.cursor or 1, scope = state.scope or "all" }
  end,
  slot = NAME,
  order = 65,
  focusable = true,

  actions = {
    {
      name = OPEN,
      desc = "open search with an optional query",
      scope = "global",
      args = {
        { name = "query", kind = "string" },
      },
    },
  },

  keys = {
    {
      key = "ctrl+/",
      action = OPEN,
      desc = "search sessions and their terminal text",
      scope = "global",
      group = "UI",
    },
    { key = "down", action = NEXT, desc = "next result (previews it)", group = "Search" },
    { key = "up", action = PREVIOUS, desc = "previous result (previews it)", group = "Search" },
    { key = "pagedown", action = PAGE_DOWN, desc = "results, a page down", group = "Search" },
    { key = "pageup", action = PAGE_UP, desc = "results, a page up", group = "Search" },
    {
      key = "enter",
      action = ACTIVATE,
      desc = "open the result, scrolled to it",
      group = "Search",
    },
    { key = "tab", action = SCOPE, desc = "search everything / text / names", group = "Search" },
    { key = "esc", action = CANCEL, desc = "close and put back", group = "Search" },
  },
  -- MUST KILL: rearchitect — moving the `want_content` and `MATCHES` writes out of render would let this pane be `pure`
  render = function(ctx)
    local width, height = ctx.width or 0, ctx.height or 0
    local search = load()
    local rows, info = results(search.scope)
    want_content(info.ask, search.scope)
    search.cursor = widgets.clamp(search.cursor, #rows)

    if memo.rows == rows and not memo.matches then
      memo.matches = matched_ids(rows)
    end
    local matches = memo.matches or matched_ids(rows)
    if store[MATCHES] ~= matches then
      store[MATCHES] = matches
    end

    local current = rows[search.cursor]
    local place = place_of(current)
    if place ~= search.previewed then
      preview(search, current, true)
      search.previewed = place
    end
    save(search)

    local children = {
      query_row(search, rows, info),
      status_line(search, rows, info),
    }

    local panel_border_rows = 2
    local consumed = panel_border_rows
    for _, child in ipairs(children) do
      consumed = consumed + (child.len or 0)
    end

    local list_height = math.max(0, height - consumed)
    local list_width = math.max(0, width - 4)
    local layout = memo.layout
    if
      not layout
      or layout.rows ~= rows
      or layout.cursor ~= search.cursor
      or layout.width ~= list_width
      or layout.height ~= list_height
      or layout.theme ~= thurbox.theme
    then
      local lines, selected_line =
        result_rows(rows, search.cursor, list_width, list_height, info, search.scope)
      layout = {
        rows = rows,
        cursor = search.cursor,
        width = list_width,
        height = list_height,
        theme = thurbox.theme,
        lines = lines,
        selected = selected_line,
      }
      memo.layout = layout
    end
    local lines, selected_line = layout.lines, layout.selected
    children[#children + 1] = widgets.list({
      rows = lines,
      selected = selected_line,
      height = list_height,
      fill = 1,
      empty = empty_text(search, info),
      hover_style = hover.row_style(),
    })

    return {
      type = "box",
      frame = chrome.frame("Search", chrome.level(ctx.focused)),
      children = children,
    }
  end,

  on_click = function(hit)
    if not hit.id then
      return false
    end
    local search = load()
    local rows = results(search.scope)
    local index = widgets.index_of(rows, hit.id)
    if not index then
      return false
    end
    search.cursor = index
    activate(search, rows[index])
    return true
  end,

  on_action = function(action, args)
    if action == OPEN and type(args) == "table" and args.query ~= nil then
      local requested = args.query
      if type(requested) ~= "string" then
        return false
      end
      local search = load()
      if not panels.shown(NAME) then
        search.snapshot = {
          selected = store.selected,
          sessions_shown = panels.shown("sessions"),
        }
        search.revealed = nil
        panels.show(NAME)
      end
      textinput.set(search.field, requested)
      search.cursor = 1
      search.previewed = nil
      store[QUERY] = requested
      save(search)
      command("focus", { text = NAME })
      return true
    end
    if action == OPEN then
      if panels.shown(NAME) then
        close(load(), false)
        command("focus", { text = "sessions" })
        return true
      end
      local search = load()
      textinput.clear(search.field)
      search.cursor = 1
      search.revealed = nil
      search.snapshot = {
        selected = store.selected,
        sessions_shown = panels.shown("sessions"),
      }
      store[QUERY] = ""
      panels.show(NAME)
      local first = results(search.scope)[1]
      preview(search, first, false)
      search.previewed = place_of(first)
      save(search)
      command("focus", { text = NAME })
      return true
    end

    if not panels.shown(NAME) then
      return false
    end

    if action == NEXT or action == PREVIOUS then
      step_cursor(action == NEXT and 1 or -1)
      return true
    elseif action == PAGE_DOWN or action == PAGE_UP then
      step_cursor(action == PAGE_DOWN and PAGE or -PAGE)
      return true
    elseif action == SCOPE then
      local search = load()
      for index, scope in ipairs(SCOPES) do
        if scope == search.scope then
          search.scope = SCOPES[index % #SCOPES + 1]
          break
        end
      end
      search.cursor = 1
      save(search)
      return true
    elseif action == ACTIVATE then
      local search = load()
      local rows = results(search.scope)
      local row = rows[widgets.clamp(search.cursor, #rows)]
      if row then
        activate(search, row)
      end
      return true
    elseif action == CANCEL then
      close(load(), false)
      command("focus", { text = "sessions" })
      return true
    end
    return false
  end,

  on_key = function(key)
    if not panels.shown(NAME) then
      return false
    end
    local search = load()
    if not textinput.key(search.field, key) then
      return false
    end
    search.cursor = 1
    save(search)
    store[QUERY] = search.field.value or ""
    return true
  end,
}
