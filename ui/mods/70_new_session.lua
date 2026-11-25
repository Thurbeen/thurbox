local fuzzy = require("lib.fuzzy")
local hover = require("lib.hover")
local modal = require("lib.modal")
local pathpicker = require("lib.pathpicker")
local repo_picker = require("lib.repo_picker")
local textinput = require("lib.textinput")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

local function bookmarks()
  return (thurbox and thurbox.bookmarks) or {}
end

local function browse()
  return (thurbox and thurbox.browse) or {}
end

local function fleets()
  return (thurbox and thurbox.fleets) or {}
end

local function several_fleets()
  return #fleets() > 1
end

local function chosen_fleet()
  local name = state.flow and state.flow.fleet
  for _, fleet in ipairs(fleets()) do
    if fleet.name == name then
      return fleet
    end
  end
  return nil
end

local function refusal()
  local fleet = chosen_fleet()
  if fleet and fleet.state == "incompatible" then
    return fleet.name .. " is " .. (fleet.why or "incompatible: update Fleet")
  end
  return nil
end

local function hosts()
  local fleet = chosen_fleet()
  if fleet then
    return fleet.places
  end
  return thurbox and thurbox.hosts
end

local function first_ready_place()
  for position, place in ipairs(hosts() or {}) do
    if place.ready ~= false then
      return position
    end
  end
  return 1
end

local function host_cursor(flow)
  return flow.host_index or first_ready_place()
end

local function known_agents()
  local fleet = chosen_fleet()
  if fleet then
    return fleet.agents
  end
  return thurbox and thurbox.agents
end

local function agents()
  return known_agents() or {}
end

local function repo_branches(repo)
  local read = thurbox and thurbox.repo_branches
  if not read or read.repo ~= repo or read.loading then
    return nil
  end
  return read
end

local function agents_known()
  return known_agents() ~= nil
end

local function spinner(flow)
  return theme.spinner_frame(flow.elapsed)
end

local function bookmark_pending()
  for _, item in ipairs((thurbox and thurbox.commands) or {}) do
    if item.kind == "bookmark" and item.phase ~= "failed" then
      return true
    end
  end
  return false
end

local function bookmark_failures()
  local failed = {}
  for _, item in ipairs((thurbox and thurbox.commands) or {}) do
    if item.kind == "bookmark" and item.phase == "failed" then
      failed[tostring(item.id)] = item.subject or ""
    end
  end
  return failed
end

local function load()
  return state.flow
end

local track_worktrees

local function save(flow)
  if track_worktrees then
    track_worktrees(flow)
  end
  state.flow = flow
end

local function fresh()
  local only = not several_fleets() and fleets()[1] or nil
  return {
    step = several_fleets() and "fleet" or "host",
    fleet = only and only.name or nil,
    remote = only ~= nil and only.remote == true,
    elsewhere = false,
    fleet_index = 1,
    host = "",
    place = "",
    cursor = 1,
    focus = "search",
    selected = {},
    worktree = {},
    collapsed = {},
    input = textinput.new(""),
    search = textinput.new(""),
    browse = false,
    browse_index = 1,
    select_newest = false,
    base = nil,
    branch_index = 1,
    wt_repo = nil,
    open_worktree = nil,
    name = textinput.new(""),
    branch = textinput.new(""),
    agent_index = 1,
    new_path = nil,
    folder_index = 1,
    url = textinput.new(""),
    pending_label = nil,
    message = nil,
  }
end

local function reads(flow)
  return flow.reads or flow.host or ""
end

local function remote_home(flow)
  local fleet = flow.remote and flow.place == "" and chosen_fleet()
  if fleet and fleet.home then
    return fleet.home
  end
  local listing = browse()
  if listing.host == reads(flow) and listing.home then
    return listing.home
  end
  return nil
end

local function place_label(flow)
  if not flow.remote then
    return flow.place ~= "" and flow.place or (flow.host or "")
  end
  if flow.place == "" then
    return flow.fleet
  end
  return flow.host
end

local function fleet_local_is_ready()
  for _, place in ipairs(hosts() or {}) do
    if (place.backend or "") == "" and place.ready ~= false then
      return true
    end
  end
  return false
end

local function repository_key(flow, place)
  local machine = flow.place == "" or place.here
  if not flow.elsewhere then
    return machine and "" or flow.place
  end
  return flow.fleet .. "/" .. (machine and "local" or place.name)
end

local function reading_key(flow, place)
  local key = repository_key(flow, place)
  if flow.elsewhere and place.here and not fleet_local_is_ready() then
    return flow.fleet .. "/" .. place.name
  end
  return key
end

local function expand_remote_home(flow, path)
  local home = remote_home(flow)
  if home and (path == "~" or path:sub(1, 2) == "~/") then
    return home .. path:sub(2)
  end
  return path
end

local function other_users_home(flow)
  if not (flow.elsewhere and flow.step == "repo") then
    return nil
  end
  local typed = (flow.input.value or ""):match("^%s*(.-)%s*$")
  if typed:match("^~[^/]") then
    return "only ~ and ~/… are supported on " .. place_label(flow)
  end
  return nil
end

local function ask(flow)
  if not flow then
    store.want_bookmarks = nil
    store.want_browse = nil
    store.want_branches = nil
    store.want_places = nil
    return
  end
  store.want_bookmarks = flow.host or ""
  store.want_places = (flow.step == "host" and flow.fleet) or nil
  if flow.step == "branch" then
    store.want_branches = flow.elsewhere and (reads(flow) .. "\0" .. flow.primary) or flow.primary
  elseif flow.step == "repo" and flow.wt_repo then
    store.want_branches = flow.wt_repo
  else
    store.want_branches = nil
  end

  local typed = flow.step == "repo" and (flow.input.value or "") or ""
  if flow.step == "repo" and flow.focus == "input" and (typed ~= "" or flow.browse) then
    local dir = pathpicker.split_typed(typed)
    if other_users_home(flow) then
      dir = nil
    elseif flow.elsewhere then
      dir = expand_remote_home(flow, dir)
      if dir:sub(1, 1) == "~" then
        dir = "~"
      end
    end
    store.want_browse = dir and (reads(flow) .. "\0" .. dir) or nil
  else
    store.want_browse = nil
  end
end

local function rows_for(flow)
  local query = flow.search.value or ""
  local entries = repo_picker.rows(bookmarks().rows or {}, query, flow.collapsed)
  local read = flow.wt_repo and repo_branches(flow.wt_repo)
  local published = read and { repo = read.repo, list = read.worktrees } or nil
  return repo_picker.with_worktrees(entries, flow.wt_repo, published)
end

local function current_row(flow)
  return repo_picker.current(rows_for(flow), flow.cursor)
end

track_worktrees = function(flow)
  if not flow or flow.step ~= "repo" or not flow.wt_repo then
    return
  end
  local entry = repo_picker.current(rows_for(flow), flow.cursor)
  local row = entry and entry.row
  if not row or (not row.is_worktree and row.path ~= flow.wt_repo) then
    flow.wt_repo = nil
    if row then
      flow.cursor = repo_picker.index_of(rows_for(flow), row.path) or 1
    end
  end
end

local function chosen(flow)
  return repo_picker.chosen(bookmarks().rows or {}, flow.selected, flow.worktree)
end

local function cursor_stand_in(flow)
  local worktrees, plain = chosen(flow)
  if #worktrees > 0 or #plain > 0 then
    return nil
  end
  local entry = current_row(flow)
  local row = entry and entry.row
  if row and not row.is_parent and not row.is_worktree then
    return row
  end
  return nil
end

local function browse_entries(flow)
  return pathpicker.entries(flow.input and flow.input.value or "", browse().entries or {})
end

local function await_new_row(flow, path)
  flow.select_newest = true
  flow.awaiting_path = path
  flow.failures_before = bookmark_failures()
  textinput.clear(flow.search)
  flow.cursor = 1
end

local function enter_path_field(flow)
  flow.focus = "input"
  if (flow.input.value or "") == "" then
    if flow.elsewhere then
      local home = remote_home(flow)
      flow.prefill = home and (home .. "/") or ""
      if not home then
        flow.message = "type an absolute path on " .. place_label(flow) .. ", or a clone URL"
      end
    else
      flow.prefill = pathpicker.start(bookmarks().rows or {})
    end
    textinput.set(flow.input, flow.prefill)
  end
end

local function untouched(flow)
  return flow.prefill ~= nil and flow.input.value == flow.prefill
end

local function typed_target(flow)
  local value = (flow.input.value or ""):match("^%s*(.-)%s*$")
  return (value:gsub("(.)/+$", "%1"))
end

local function names_missing_folder(flow)
  if untouched(flow) then
    return false
  end
  local target = typed_target(flow)
  if target == "" or target == "/" or target == "~" then
    return false
  end
  local dir, leaf = pathpicker.split_typed(target)
  if flow.elsewhere then
    return false
  end
  local listing = browse()
  if listing.dir ~= dir or (listing.host or "") ~= reads(flow) or listing.loading then
    return false
  end
  if listing.error then
    return true
  end
  for _, entry in ipairs(listing.entries or {}) do
    if entry.name == leaf then
      return false
    end
  end
  return true
end

local function suggestion_for(flow)
  return pathpicker.suggestion(flow.input and flow.input.value or "", browse().entries or {})
end

local MODAL_COLS = 60
local ROW_COLS = MODAL_COLS - 4

local function leaf(path)
  if not path or path == "" then
    return ""
  end
  return (path:gsub("/+$", ""):match("[^/]+$")) or path
end

local function trail(flow)
  if not flow then
    return ""
  end
  local parts = {}
  if several_fleets() and flow.fleet and not flow.remote and flow.step ~= "fleet" then
    parts[#parts + 1] = flow.fleet
  end
  if (flow.host or "") ~= "" then
    parts[#parts + 1] = flow.remote and flow.host or place_label(flow)
  end
  if (flow.primary or "") ~= "" then
    local extra = #(flow.extras or {})
    parts[#parts + 1] = leaf(flow.primary) .. (extra > 0 and (" +" .. extra) or "")
  end
  if #parts == 0 then
    return ""
  end
  return table.concat(parts, " › ")
end

local function frame(title, rows_height, children, flow, cols)
  return modal.frame(title, {
    cols = cols or MODAL_COLS,
    rows = rows_height,
    children = children,
    crumbs = trail(flow),
  })
end

local function message_row(flow)
  local text = refusal() or flow.message or other_users_home(flow)
  return {
    type = "text",
    len = 1,
    text = { { { text = text and (" " .. text) or "", style = { fg = theme.bad } } } },
  }
end

local function row_hover(id, is_cursor)
  if not is_cursor and hover.id(id) then
    return hover.row_style()
  end
  return nil
end

local function selector_rows(labels, index, height, dimmed)
  local children = {}
  local first, last = widgets.window(#labels, height, index)
  for position = first, last do
    local selected = position == index
    local id = tostring(position)
    local colour = (dimmed and dimmed[position]) and theme.muted
      or (selected and theme.accent or theme.text)
    children[#children + 1] = {
      type = "text",
      len = 1,
      text = {
        {
          {
            text = (selected and "▸ " or "  ") .. labels[position],
            style = { fg = colour, bold = selected },
          },
        },
      },
      style = row_hover(id, selected),
      id = id,
      role = "row",
    }
  end
  return children
end

local GUTTER = 2

local function measure(rows)
  local widths = {}
  for _, cells in ipairs(rows) do
    for column, cell in ipairs(cells) do
      widths[column] = math.max(widths[column] or 0, widgets.len(cell))
    end
  end
  local natural = 0
  for column, cells in ipairs(widths) do
    natural = natural + cells + (column > 1 and GUTTER or 0)
  end
  return widths, natural
end

local function table_labels(rows, width, shrink)
  local widths, natural = measure(rows)
  for _, column in ipairs(shrink) do
    local over = natural - width
    if over > 0 and widths[column] then
      local cut = math.min(over, math.max(0, widths[column] - 1))
      widths[column] = widths[column] - cut
      natural = natural - cut
    end
  end
  local labels = {}
  for _, cells in ipairs(rows) do
    local parts = {}
    for column, cell in ipairs(cells) do
      local fitted = widgets.truncate(cell, widths[column])
      parts[column] = column < #cells and widgets.pad(fitted, widths[column]) or fitted
    end
    labels[#labels + 1] = widgets.truncate(table.concat(parts, string.rep(" ", GUTTER)), width)
  end
  return labels
end

local function wrap(text, width)
  local lines, line = {}, ""
  for word in text:gmatch("%S+") do
    word = widgets.truncate(word, width)
    if line == "" then
      line = word
    elseif widgets.len(line) + 1 + widgets.len(word) <= width then
      line = line .. " " .. word
    else
      lines[#lines + 1] = line
      line = word
    end
  end
  if line ~= "" then
    lines[#lines + 1] = line
  end
  return lines
end

local REASON_LINES = 4

local function picker_cols(natural, flow)
  local screen = flow.screen_cols or MODAL_COLS
  local cols = math.min(math.max(MODAL_COLS, natural + 4), screen)
  return cols, cols - 4
end

local function reason_height(reasons, width)
  local height = 0
  for _, reason in ipairs(reasons) do
    height = math.max(height, math.min(#wrap(reason, width), REASON_LINES))
  end
  return height
end

local function picker_rows(count, wanted_reason, flow)
  local avail = (flow.screen_rows or math.huge) - 4
  local reason = math.min(wanted_reason, math.max(math.min(wanted_reason, 1), avail - count))
  return math.max(1, math.min(count, avail - reason)), reason
end

local function reason_rows(reasons, index, width, height)
  local lines = wrap(reasons[index] or "", width)
  if #lines > 1 and height == 1 then
    lines = { widgets.truncate(table.concat(lines, " "), width) }
  elseif #lines > height and height > 0 then
    lines[height] = widgets.truncate(lines[height] .. " …", width)
  end
  local text = {}
  for line = 1, height do
    text[line] = { { text = " " .. (lines[line] or ""), style = { fg = theme.muted } } }
  end
  return { type = "text", len = height, text = text }
end

local FLEET_STATES = {
  up = "● up",
  down = "○ Fleet not running",
  unreachable = "○ unreachable",
  connecting = "◌ connecting…",
  unknown = "? unknown",
  incompatible = "⊘ incompatible",
}

local function fleet_detail(fleet)
  if fleet.state == "down" and fleet.remote then
    return "install the systemd unit (see doctor)"
  end
  return fleet.why or ""
end

local function render_fleet(flow)
  local rows, reasons = {}, {}
  for _, fleet in ipairs(fleets()) do
    rows[#rows + 1] =
      { fleet.name, fleet.transport or "", FLEET_STATES[fleet.state] or fleet.state or "" }
    reasons[#reasons + 1] = fleet_detail(fleet)
  end
  if flow.fleet_index > #rows then
    flow.fleet_index = math.max(#rows, 1)
    save(flow)
  end
  local _, natural = measure(rows)
  local cols, row_cols = picker_cols(natural, flow)
  local labels = table_labels(rows, row_cols, { 2, 1 })
  local height, reason_lines = picker_rows(#labels, reason_height(reasons, cols - 3), flow)
  local reason = reason_rows(reasons, flow.fleet_index, cols - 3, reason_lines)
  return frame("Fleet", height + reason_lines + 4, {
    { type = "box", len = height, children = selector_rows(labels, flow.fleet_index, height) },
    reason,
    message_row(flow),
    modal.footer({ { "j/k", "navigate" } }, "Select"),
  }, flow, cols)
end

local function readiness(place)
  if place.ready == true then
    return "● ready"
  elseif place.ready == false then
    return "○ not ready"
  end
  return ""
end

local function render_host(flow)
  local places = hosts() or {}
  local rows, reasons, dimmed = {}, {}, {}
  for position, place in ipairs(places) do
    rows[#rows + 1] = { place.name, place.detail or "", readiness(place) }
    reasons[#reasons + 1] = place.ready == false and (place.why or "") or ""
    dimmed[position] = place.ready == false
  end
  local refused = refusal()
  if refused then
    rows, reasons, dimmed = { { refused } }, {}, {}
  elseif hosts() == nil then
    rows = { { "Asking " .. (flow.fleet or "Fleet") .. " for its places…" } }
  end
  if flow.host_index and flow.host_index > #rows then
    flow.host_index = math.max(#rows, 1)
    save(flow)
  end
  local index = math.min(host_cursor(flow), math.max(#rows, 1))
  local _, natural = measure(rows)
  local cols, row_cols = picker_cols(natural, flow)
  local labels = table_labels(rows, row_cols, { 2, 1 })
  local height, reason_lines =
    picker_rows(#labels, math.min(reason_height(reasons, cols - 3), 1), flow)
  local reason = reason_rows(reasons, index, cols - 3, reason_lines)
  local title = several_fleets() and flow.fleet and ("Place on " .. flow.fleet) or "Place"
  return frame(title, height + reason_lines + 4, {
    { type = "box", len = height, children = selector_rows(labels, index, height, dimmed) },
    reason,
    message_row(flow),
    modal.footer({ { "j/k", "navigate" } }, "Select", { disabled = dimmed[index] }),
  }, flow, cols)
end

local REPO_LIST_MAX = 10
local BROWSE_MAX = 8

local SEARCH_FIELD = "field:search"
local PATH_FIELD = "field:input"
local BROWSE_ROW = "browse:"
local REPO_ROW = "repo:"

local function repo_row(entry, selected, flow, is_cursor)
  local row = entry.row
  local style = is_cursor and { fg = theme.accent, bold = true } or { fg = theme.text }
  if row.is_parent then
    return {
      { text = (flow.collapsed[row.path] and "▸ " or "▾ ") .. row.path, style = style },
      { text = " (parent)", style = { fg = theme.muted } },
    }
  end

  if row.is_worktree then
    return {
      { text = "  ↳ ", style = { fg = theme.muted } },
      { text = row.name, style = style },
      {
        text = "  " .. widgets.middle_truncate(row.branch or "", math.max(8, ROW_COLS - 30)),
        style = { fg = theme.muted },
      },
    }
  end

  local spans = {}
  if row.parent then
    spans[#spans + 1] = { text = "  ", style = style }
  end
  spans[#spans + 1] = { text = selected and "[x] " or "[ ] ", style = style }
  if row.label then
    spans[#spans + 1] = { text = row.label .. "  ", style = style }
  end
  if #entry.matched > 0 then
    local hit = { fg = theme.accent_bright, bold = true }
    for _, span in ipairs(fuzzy.spans(row.path, entry.matched, style, hit)) do
      spans[#spans + 1] = span
    end
  else
    local used = 4 + (row.parent and 2 or 0) + (row.label and (widgets.len(row.label) + 2) or 0)
    local avail = math.max(8, ROW_COLS - used)
    spans[#spans + 1] = {
      text = widgets.middle_truncate(row.path, avail),
      style = row.label and { fg = theme.muted } or style,
    }
  end

  if selected and flow.worktree[row.path] then
    spans[#spans + 1] = { text = " [wt]", style = { fg = theme.accent } }
  end
  if row.is_git == false then
    spans[#spans + 1] = { text = " (dir)", style = { fg = theme.muted } }
  end
  return spans
end

local function spawns_directly()
  return agents_known() and #agents() <= 1
end

local function render_repo(flow)
  local entries = rows_for(flow)
  local total = #(bookmarks().rows or {})
  local visible = math.max(1, math.min(#entries, REPO_LIST_MAX))
  local query = flow.search.value or ""
  local dropdown = flow.browse and browse() or nil

  local children = {}

  children[#children + 1] = textinput.node(flow.search, {
    label = "Search (" .. #entries .. "/" .. total .. ")",
    focused = flow.focus == "search",
    id = SEARCH_FIELD,
  })

  local list = {}
  if #entries == 0 then
    list[1] = {
      type = "text",
      len = 1,
      text = {
        {
          {
            text = query ~= "" and "  No matches" or "  No bookmarks — add via path input below",
            style = { fg = theme.muted },
          },
        },
      },
    }
  else
    local cursor = widgets.clamp(flow.cursor, #entries)
    local first, last = widgets.window(#entries, visible, cursor)
    for position = first, last do
      local entry = entries[position]
      local path = entry.row.path
      local is_cursor = position == cursor and flow.focus == "search"
      local id = REPO_ROW .. path
      list[#list + 1] = {
        type = "text",
        len = 1,
        text = { repo_row(entry, flow.selected[path] == true, flow, is_cursor) },
        style = row_hover(id, is_cursor),
        id = id,
        role = "row",
      }
    end
  end

  local host_name = flow.remote and (flow.host or "") or place_label(flow)
  children[#children + 1] = {
    type = "box",
    len = visible + 2,
    frame = {
      title = host_name ~= "" and (" Repos on " .. host_name .. " (" .. total .. ") ")
        or (" Repos (" .. total .. ") "),
      borders = "all",
      border_style = {
        fg = flow.focus == "search" and theme.border_focused or theme.border,
      },
    },
    children = list,
  }

  local suggestion = flow.suggestion or ""
  local label = flow.elsewhere and "Add Repo Path or Clone URL" or "Add Repo Path"
  if bookmark_pending() then
    label = label .. " " .. spinner(flow) .. " " .. (flow.pending_label or "checking…")
  end
  children[#children + 1] = {
    type = "box",
    axis = "horizontal",
    len = 3,
    frame = {
      title = " " .. label .. " ",
      borders = "all",
      border_style = { fg = hover.border(flow.focus == "input", PATH_FIELD) },
    },
    id = PATH_FIELD,
    children = {
      {
        type = "input",
        len = widgets.len(flow.input.value or "") + 1,
        value = flow.input.value or "",
        cursor = flow.input.cursor or 0,
        placeholder = "",
        focused = flow.focus == "input",
        style = { fg = theme.text },
      },
      {
        type = "text",
        fill = 1,
        text = { { { text = suggestion, style = { fg = theme.muted } } } },
      },
    },
  }

  if dropdown then
    local rows = {}
    local entries_shown = browse_entries(flow)
    if dropdown.loading then
      rows[1] = {
        type = "text",
        len = 1,
        text = {
          { { text = " " .. spinner(flow) .. " listing…", style = { fg = theme.muted } } },
        },
      }
    elseif dropdown.error then
      rows[1] = {
        type = "text",
        len = 1,
        text = { { { text = " " .. dropdown.error, style = { fg = theme.bad } } } },
      }
    elseif #entries_shown == 0 then
      rows[1] = {
        type = "text",
        len = 1,
        text = { { { text = " (no subdirectories)", style = { fg = theme.muted } } } },
      }
    else
      local index = widgets.clamp(flow.browse_index, #entries_shown)
      local height = math.min(#entries_shown, BROWSE_MAX)
      local first, last = widgets.window(#entries_shown, height, index)
      for position = first, last do
        local entry = entries_shown[position]
        local selected = position == index
        local id = BROWSE_ROW .. entry.name
        local spans = {
          {
            text = " " .. entry.name .. "/",
            style = selected and { fg = theme.accent, bold = true } or { fg = theme.text },
          },
        }
        if entry.is_git then
          spans[#spans + 1] = { text = " ●git", style = { fg = theme.accent } }
        end
        rows[#rows + 1] = {
          type = "text",
          len = 1,
          text = { spans },
          style = row_hover(id, selected),
          id = id,
          role = "row",
        }
      end
    end
    children[#children + 1] = {
      type = "box",
      len = #rows + 2,
      frame = {
        title = " Browse " .. (dropdown.dir or "") .. " ",
        borders = "all",
        border_style = { fg = theme.border_focused },
      },
      children = rows,
    }
  end

  children[#children + 1] = message_row(flow)

  local hints, primary, cancel
  if flow.focus == "search" then
    hints = {
      { "↑/↓", "nav" },
      { "space", "tick" },
      { "alt+w", "worktree" },
      { "del", "forget" },
      { "tab", "path" },
    }
    local entry = entries[widgets.clamp(flow.cursor, #entries)]
    if entry and entry.row.is_worktree then
      primary = spawns_directly() and "Open" or "Next"
    else
      local worktrees, plain = chosen(flow)
      local nothing_to_carry = #worktrees == 0 and #plain == 0 and not cursor_stand_in(flow)
      if nothing_to_carry and (flow.host or "") ~= "" then
        primary = nil
      else
        primary = "Next"
      end
    end
    if query ~= "" then
      cancel = "Clear"
    end
  elseif dropdown then
    hints = {
      { "↑/↓", "select" },
      { "tab", "open folder" },
      { "s-tab", "search" },
      { "esc", "close" },
    }
    local shown = browse_entries(flow)
    local entry = shown[widgets.clamp(flow.browse_index, #shown)]
    primary = entry and (entry.is_git and "Add repo" or "Add directory") or nil
    cancel = "Close"
  else
    hints = {
      { "tab", suggestion ~= "" and "complete" or "browse" },
      { "alt+p", "import parent" },
      { "s-tab", "search" },
      { "alt+⏎", "next" },
    }
    if flow.elsewhere then
      hints = { hints[1], { "s-tab", "search" }, { "alt+⏎", "next" } }
    end
    local typed = (flow.input.value or ""):match("^%s*(.-)%s*$")
    if typed == "" or untouched(flow) then
      primary = nil
    elseif names_missing_folder(flow) then
      primary = "Create folder"
    else
      primary = "Add repo"
    end
  end
  if flow.focus == "search" and (flow.host or "") == "" then
    children[#children + 1] = {
      type = "text",
      len = 1,
      text = "← close worktrees  → expand worktrees",
      style = { fg = theme.muted },
    }
  end
  children[#children + 1] = modal.footer(hints, primary, { cancel = cancel, stack = true })

  local height = 2
  for _, child in ipairs(children) do
    height = height + (child.len or 1)
  end
  return frame("Select Repos", height, children, flow)
end

local FOLDER_CHOICES = {
  { action = "init", label = "New git repository (git init)" },
  { action = "clone", label = "Clone a repository into it" },
  { action = "create", label = "Leave it empty" },
}

local function render_new_folder(flow)
  local labels = {}
  for index, choice in ipairs(FOLDER_CHOICES) do
    labels[index] = choice.label
  end
  local height = #labels
  return frame("New Folder", height + 6, {
    {
      type = "text",
      len = 1,
      text = {
        {
          { text = " Create ", style = { fg = theme.muted } },
          {
            text = widgets.middle_truncate(flow.new_path or "", ROW_COLS - 8),
            style = { fg = theme.text },
          },
        },
      },
    },
    { type = "text", len = 1, text = "" },
    { type = "box", len = height, children = selector_rows(labels, flow.folder_index, height) },
    message_row(flow),
    modal.footer({ { "↑/↓", "choose" } }, "Create", { cancel = "Back" }),
  }, flow)
end

local function render_branch(flow)
  local read = repo_branches(flow.primary)
  local names = (read and read.names) or {}
  if not read or #names == 0 then
    local pending = flow.elsewhere and (" listing branches on " .. place_label(flow) .. "…")
      or " fetching and listing branches…"
    local text = (read and read.error) or (spinner(flow) .. pending)
    local failed = read and read.error ~= nil
    return frame("Base Branch", 5, {
      {
        type = "text",
        len = 1,
        text = {
          { { text = " " .. text, style = { fg = failed and theme.bad or theme.muted } } },
        },
      },
      message_row(flow),
      modal.footer({ { "esc", "cancel" } }, nil),
    }, flow)
  end
  if not flow.branch_placed then
    flow.branch_placed = true
    flow.branch_index = 1
    for index, name in ipairs(names) do
      if name == read.default then
        flow.branch_index = index
      end
    end
    save(flow)
  end
  local height = math.min(#names, REPO_LIST_MAX)
  local rows = {}
  local note = read.fetch_error and (" not fetched: " .. read.fetch_error)
    or (flow.elsewhere and (" branches as of " .. place_label(flow) .. "'s last fetch"))
  if note then
    rows[1] = {
      type = "text",
      len = 1,
      text = {
        {
          {
            text = widgets.middle_truncate(note, ROW_COLS),
            style = { fg = read.fetch_error and theme.bad or theme.muted },
          },
        },
      },
    }
  end
  rows[#rows + 1] = {
    type = "box",
    len = height,
    children = selector_rows(names, widgets.clamp(flow.branch_index, #names), height),
  }
  rows[#rows + 1] = message_row(flow)
  rows[#rows + 1] = modal.footer({ { "j/k", "navigate" } }, "Select")
  return frame("Base Branch", height + 4 + (note and 1 or 0), rows, flow)
end

local function render_clone(flow)
  local url = (flow.url.value or ""):match("^%s*(.-)%s*$")
  return frame("Clone Repository", 8, {
    textinput.node(flow.url, {
      label = "Repository URL",
      focused = true,
      placeholder = "git@host:owner/repo.git or https://…",
    }),
    {
      type = "text",
      len = 1,
      text = {
        {
          { text = " Into ", style = { fg = theme.muted } },
          {
            text = widgets.middle_truncate(flow.new_path or "", ROW_COLS - 6),
            style = { fg = theme.text },
          },
        },
      },
    },
    message_row(flow),
    modal.footer({ { "esc", "back" } }, url ~= "" and "Clone" or nil, { cancel = "Back" }),
  }, flow)
end

local function field_creates(flow)
  if flow.step == "name" and flow.base then
    return false
  end
  return spawns_directly()
end

local function render_field(title, label, field, flow, placeholder)
  local resolved = (field.value or ""):match("^%s*(.-)%s*$")
  if resolved == "" then
    resolved = placeholder or ""
  end
  return frame(title, 7, {
    textinput.node(field, {
      label = label,
      focused = true,
      placeholder = placeholder,
    }),
    message_row(flow),
    modal.footer(
      { { "enter", "confirm" }, { "esc", "cancel" } },
      resolved ~= "" and (field_creates(flow) and "Create" or "Next") or nil
    ),
  }, flow)
end

local function render_agent(flow)
  if refusal() then
    return frame("Coding Agent", 4, {
      message_row(flow),
      modal.footer({ { "esc", "cancel" } }, nil),
    }, flow)
  end
  if not agents_known() then
    return frame("Coding Agent", 5, {
      {
        type = "text",
        len = 1,
        text = {
          {
            {
              text = " " .. spinner(flow) .. " Asking Fleet for its agents…",
              style = { fg = theme.muted },
            },
          },
        },
      },
      message_row(flow),
      modal.footer({ { "esc", "cancel" } }, nil),
    }, flow)
  end
  local labels = {}
  for _, agent in ipairs(agents()) do
    labels[#labels + 1] = agent.name
  end
  local height = math.max(1, math.min(#labels, REPO_LIST_MAX))
  return frame("Coding Agent", height + 4, {
    {
      type = "box",
      len = height,
      children = selector_rows(labels, widgets.clamp(flow.agent_index, #labels), height),
    },
    message_row(flow),
    modal.footer({ { "j/k", "navigate" } }, "Create"),
  })
end

local function suggested_name(flow)
  local name = leaf(flow and flow.primary)
  if name == "" or name == "~" then
    return ""
  end
  return name
end

local function branch_from_name(name)
  local out = {}
  for _, code in utf8.codes(name) do
    local char = utf8.char(code)
    if char:match("[%w]") then
      out[#out + 1] = char:lower()
    elseif (char == " " or char == "-" or char == "_") and out[#out] ~= "-" then
      out[#out + 1] = "-"
    end
  end
  return (table.concat(out):gsub("^%-+", ""):gsub("%-+$", ""))
end

local function after_repos(flow)
  local worktrees, plain = chosen(flow)
  flow.extras = {}
  flow.base = nil
  if #worktrees > 0 then
    flow.primary = worktrees[1]
    for index = 2, #worktrees do
      flow.extras[#flow.extras + 1] = { path = worktrees[index], worktree = true }
    end
    for _, path in ipairs(plain) do
      flow.extras[#flow.extras + 1] = { path = path, worktree = false }
    end
    flow.step = "branch"
    flow.branch_index = 1
    flow.branch_placed = false
    return flow
  end
  if #plain > 0 then
    flow.primary = plain[1]
    for index = 2, #plain do
      flow.extras[#flow.extras + 1] = { path = plain[index], worktree = false }
    end
    flow.step = "name"
    return flow
  end
  if (flow.host or "") ~= "" then
    flow.message = "Pick a repository on " .. place_label(flow)
    return flow
  end
  flow.primary = "~"
  flow.step = "name"
  return flow
end

local function commit(flow)
  local picked = agents()[flow.agent_index]
  local open = flow.open_worktree
  local worktree = flow.base ~= nil and not open
  command("create", {
    text = (open and leaf(open.path)) or flow.name.value,
    repo = (open and open.path) or flow.primary,
    worktree = worktree,
    branch = worktree and flow.branch.value or nil,
    base = worktree and flow.base or nil,
    agent = picked and picked.name or nil,
    host = (flow.place ~= "" and flow.place) or nil,
    fleet = flow.fleet,
    extras = (not open and flow.extras) or {},
  })
  save(nil)
  ask(nil)
end

local function after_name(flow)
  local list = agents()
  if agents_known() and #list <= 1 then
    flow.agent_index = 1
    commit(flow)
    return nil
  end
  flow.step = "agent"
  flow.agent_index = 1
  return flow
end

local function confirm_repos(flow)
  local stand_in = cursor_stand_in(flow)
  if stand_in then
    flow.selected[stand_in.path] = true
  end
  local entry = current_row(flow)
  local row = entry and entry.row
  local worktrees, plain = chosen(flow)
  if row and row.is_worktree and #worktrees == 0 and #plain == 0 then
    flow.primary = row.parent
    flow.extras = {}
    flow.open_worktree = { path = row.path, branch = row.branch }
    save(after_name(flow))
  else
    save(after_repos(flow))
  end
  ask(load())
end

local function dropdown_open(flow)
  return flow.browse == true and flow.focus == "input"
end

local function move_selection(flow, step)
  if flow.step == "fleet" then
    flow.fleet_index = widgets.clamp(flow.fleet_index + step, #fleets())
  elseif flow.step == "host" then
    flow.host_index = widgets.clamp(host_cursor(flow) + step, #(hosts() or {}))
  elseif flow.step == "repo" then
    flow.cursor = widgets.clamp((flow.cursor or 1) + step, #rows_for(flow))
  elseif flow.step == "agent" then
    flow.agent_index = widgets.clamp(flow.agent_index + step, #agents())
  elseif flow.step == "new_folder" then
    flow.folder_index = widgets.clamp(flow.folder_index + step, #FOLDER_CHOICES)
  elseif flow.step == "branch" then
    local read = repo_branches(flow.primary)
    flow.branch_index = widgets.clamp(flow.branch_index + step, #((read and read.names) or {}))
  end
end

return {
  name = "new_session",
  capabilities = { "sessions" },
  ui_state = function()
    local flow = load()
    if not flow then
      return { open = false }
    end
    local selection = flow.cursor or 1
    if flow.step == "fleet" then
      selection = flow.fleet_index
    elseif flow.step == "host" then
      selection = host_cursor(flow)
    elseif flow.step == "agent" then
      selection = flow.agent_index
    elseif flow.step == "new_folder" then
      selection = flow.folder_index
    elseif flow.step == "branch" then
      selection = flow.branch_index
    end
    return { open = true, step = flow.step, selection = selection }
  end,
  slot = "float",
  order = 70,
  floats = true,
  focusable = false,
  pure = true,

  keys = {
    {
      key = "ctrl+n",
      action = "new_session.open",
      desc = "new session",
      scope = "global",
      group = "Sessions",
    },
    { key = "j", action = "new_session.next", desc = "next choice", group = "New session" },
    { key = "k", action = "new_session.previous", desc = "previous choice", group = "New session" },
    { key = "down", action = "new_session.next", desc = "next choice", group = "New session" },
    {
      key = "up",
      action = "new_session.previous",
      desc = "previous choice",
      group = "New session",
    },
    {
      key = "space",
      action = "new_session.toggle",
      desc = "select a repository, or fold a folder",
      group = "New session",
    },
    {
      key = "alt+w",
      action = "new_session.worktree",
      desc = "give a repository its own worktree",
      group = "New session",
    },
    {
      key = "alt+d",
      action = "new_session.forget",
      desc = "forget a remembered repository",
      group = "New session",
    },
    {
      key = "delete",
      action = "new_session.forget",
      desc = "forget a remembered repository",
      group = "New session",
    },
    {
      key = "ctrl+enter",
      action = "new_session.confirm",
      desc = "go on with the ticked repositories, or the one under the cursor",
      group = "New session",
    },
    {
      key = "alt+enter",
      action = "new_session.confirm",
      desc = "go on with the ticked repositories, or the one under the cursor",
      group = "New session",
    },
    {
      key = "alt+p",
      action = "new_session.import",
      desc = "import a folder of repositories",
      group = "New session",
    },
  },

  render = function(ctx)
    local flow = load()
    ask(flow)
    if not flow then
      return { type = "text", text = "" }
    end

    flow.elapsed = ctx.elapsed
    flow.screen_cols = ctx.width
    flow.screen_rows = ctx.height
    flow.suggestion = flow.step == "repo" and flow.focus == "input" and suggestion_for(flow) or ""

    if flow.select_newest and not bookmark_pending() and not bookmarks().loading then
      local failed = false
      for _, item in ipairs((thurbox and thurbox.commands) or {}) do
        if
          item.kind == "bookmark"
          and item.phase == "failed"
          and item.subject == flow.awaiting_path
          and not (flow.failures_before or {})[tostring(item.id)]
        then
          failed = true
          flow.message = item.error or ("Could not add directory: " .. flow.awaiting_path)
          if untouched(flow) or (flow.input.value or "") == "" then
            textinput.set(flow.input, flow.awaiting_path)
            flow.prefill = nil
          end
          flow.focus = "input"
        end
      end
      local newest = not failed and repo_picker.newest(bookmarks().rows or {})
      if newest then
        flow.selected[newest.path] = true
        flow.cursor = repo_picker.index_of(rows_for(flow), newest.path) or 1
      end
      flow.select_newest = nil
      flow.awaiting_path = nil
      flow.failures_before = nil
      flow.pending_label = nil
      save(flow)
    end

    if flow.step == "fleet" then
      return render_fleet(flow)
    elseif flow.step == "host" then
      return render_host(flow)
    elseif flow.step == "repo" then
      return render_repo(flow)
    elseif flow.step == "branch" then
      return render_branch(flow)
    elseif flow.step == "name" then
      return render_field("Session Name", "Name", flow.name, flow, suggested_name(flow))
    elseif flow.step == "worktree" then
      return render_field("Branch Name", "Branch", flow.branch, flow)
    elseif flow.step == "new_folder" then
      return render_new_folder(flow)
    elseif flow.step == "clone" then
      return render_clone(flow)
    end
    return render_agent(flow)
  end,

  on_action = function(action)
    if action == "new_session.open" then
      if load() then
        return true
      end
      local flow = fresh()
      save(flow)
      ask(flow)
      return true
    end

    local flow = load()
    if not flow then
      return false
    end
    local typing = flow.step == "repo"
      or flow.step == "name"
      or flow.step == "worktree"
      or flow.step == "clone"
    local in_search = flow.step == "repo" and flow.focus == "search"
    flow.message = nil

    if action == "new_session.next" or action == "new_session.previous" then
      if typing then
        return false
      end
      move_selection(flow, action == "new_session.next" and 1 or -1)
      save(flow)
      return true
    end

    if flow.step ~= "repo" then
      return false
    end

    if action == "new_session.confirm" then
      flow.browse = false
      confirm_repos(flow)
      return true
    elseif action == "new_session.toggle" then
      if not in_search then
        return false
      end
      local entry = current_row(flow)
      if entry and not entry.row.is_worktree then
        local path = entry.row.path
        if entry.row.is_parent then
          flow.collapsed[path] = not flow.collapsed[path] or nil
        else
          flow.selected[path] = not flow.selected[path] or nil
          if not flow.selected[path] then
            flow.worktree[path] = nil
          end
        end
      end
      save(flow)
      return true
    elseif action == "new_session.worktree" then
      if dropdown_open(flow) then
        return false
      end
      local entry = current_row(flow)
      if entry and not entry.row.is_parent and not entry.row.is_worktree then
        if entry.row.is_git == false then
          flow.message =
            "Not a git repo — worktree mode needs one (it can still be added as a plain dir)"
        elseif (flow.host or "") ~= "" and not flow.elsewhere then
          flow.message = "Worktrees need a Fleet's place — it runs as it is"
        else
          local path = entry.row.path
          flow.worktree[path] = not flow.worktree[path] or nil
          if flow.worktree[path] then
            flow.selected[path] = true
          end
        end
      end
      save(flow)
      return true
    elseif action == "new_session.forget" then
      local ahead = widgets.chars(flow.search.value or "") > (flow.search.cursor or 0)
      if not in_search or (ahead and textinput.key(flow.search, { key = "delete" })) then
        save(flow)
        return in_search
      end
      local entry = current_row(flow)
      if entry then
        if entry.row.parent then
          flow.message = "Part of a folder — forget the folder header instead"
        else
          command("bookmark", { host = flow.host, repo = entry.row.path, action = "remove" })
          flow.selected[entry.row.path] = nil
          flow.worktree[entry.row.path] = nil
        end
      end
      save(flow)
      return true
    elseif action == "new_session.import" then
      local path = (flow.input.value or ""):match("^%s*(.-)%s*$")
      if path == "" then
        flow.message = "Type a folder path, then alt+p to import its repos as a parent"
      else
        command("bookmark", { host = flow.host, repo = path, action = "parent" })
        textinput.clear(flow.input)
        flow.browse = false
        flow.focus = "search"
      end
      save(flow)
      return true
    end
    return false
  end,

  on_key = function(key)
    local flow = load()
    if not flow then
      return false
    end
    local name = key.key

    if (name == "down" or name == "up") and not dropdown_open(flow) then
      move_selection(flow, name == "down" and 1 or -1)
      save(flow)
      return true
    end

    if name == "esc" then
      if flow.step == "repo" and flow.browse then
        flow.browse = false
      elseif flow.step == "repo" and flow.focus == "search" and flow.search.value ~= "" then
        textinput.clear(flow.search)
        flow.cursor = 1
      elseif flow.step == "new_folder" then
        flow.step = "repo"
        flow.focus = "input"
      elseif flow.step == "clone" then
        flow.step = "new_folder"
      elseif flow.step == "host" and several_fleets() then
        flow.step = "fleet"
      else
        save(nil)
        ask(nil)
        return true
      end
      save(flow)
      ask(flow)
      return true
    end

    flow.message = nil

    if flow.step == "fleet" then
      if name == "enter" then
        local chosen = fleets()[flow.fleet_index]
        if not chosen then
          return true
        end
        if chosen.state == "unreachable" then
          flow.message = chosen.name .. " cannot be reached: " .. (chosen.why or "see doctor")
          save(flow)
          return true
        end
        if chosen.state == "incompatible" then
          flow.message = chosen.name .. " is " .. (chosen.why or "incompatible: update Fleet")
          save(flow)
          return true
        end
        flow.fleet = chosen.name
        flow.remote = chosen.remote == true
        flow.step = "host"
        flow.host_index = nil
        save(flow)
        return true
      end
      return false
    end

    if flow.step == "host" then
      if name == "enter" then
        local refused = refusal()
        if refused then
          flow.message = refused
          save(flow)
          return true
        end
        local chosen = (hosts() or {})[host_cursor(flow)]
        if not chosen then
          return true
        end
        if chosen.ready == false then
          flow.message = chosen.name .. " is not ready"
          save(flow)
          return true
        end
        flow.place = chosen.backend or ""
        flow.elsewhere = flow.fleet ~= nil
          and (flow.remote or not (flow.place == "" or chosen.here))
        flow.host = repository_key(flow, chosen)
        flow.reads = reading_key(flow, chosen)
        flow.step = "repo"
        flow.cursor = 1
        flow.selected, flow.worktree, flow.collapsed = {}, {}, {}
        save(flow)
        ask(flow)
        return true
      end
      return false
    end

    if flow.step == "branch" then
      if name == "enter" then
        local read = repo_branches(flow.primary)
        local names = (read and read.names) or {}
        local chosen_branch = names[widgets.clamp(flow.branch_index, #names)]
        if chosen_branch then
          flow.base = chosen_branch
          flow.step = "name"
          save(flow)
        end
        return true
      end
      return false
    end

    if flow.step == "name" or flow.step == "worktree" then
      local field = flow.step == "name" and flow.name or flow.branch
      if name == "enter" then
        local value = (field.value or ""):match("^%s*(.-)%s*$")
        if value == "" and flow.step == "name" then
          value = suggested_name(flow)
          field.value = value
          field.cursor = #value
        end
        if value == "" then
          flow.message = flow.step == "name" and "Session name cannot be empty"
            or "Branch name cannot be empty"
          save(flow)
          return true
        end
        if flow.step == "name" then
          flow.name.value = value
          if flow.base then
            textinput.set(flow.branch, branch_from_name(value))
            flow.step = "worktree"
            save(flow)
            return true
          end
        else
          flow.branch.value = value
        end
        save(after_name(flow))
        return true
      end
      if textinput.key(field, key) then
        save(flow)
        return true
      end
      return false
    end

    if flow.step == "agent" then
      if name == "enter" then
        if agents_known() then
          commit(flow)
        end
        return true
      end
      return false
    end

    if flow.step == "clone" then
      if name == "enter" then
        local url = (flow.url.value or ""):match("^%s*(.-)%s*$")
        if url == "" then
          flow.message = "Paste the URL of the repository to clone"
          save(flow)
          return true
        end
        command("bookmark", {
          host = flow.host,
          repo = flow.new_path,
          action = "clone",
          text = url,
        })
        flow.step = "repo"
        flow.focus = "search"
        await_new_row(flow, flow.new_path)
        flow.pending_label = "cloning…"
        textinput.clear(flow.input)
        save(flow)
        ask(flow)
        return true
      end
      if textinput.key(flow.url, key) then
        save(flow)
        return true
      end
      return false
    end

    if flow.step == "new_folder" then
      if name == "enter" then
        local choice = FOLDER_CHOICES[widgets.clamp(flow.folder_index, #FOLDER_CHOICES)]
        if choice.action == "clone" then
          flow.step = "clone"
          textinput.clear(flow.url)
          save(flow)
          return true
        end
        command("bookmark", { host = flow.host, repo = flow.new_path, action = choice.action })
        flow.step = "repo"
        flow.focus = "search"
        await_new_row(flow, flow.new_path)
        textinput.clear(flow.input)
        save(flow)
        ask(flow)
        return true
      end
      return false
    end

    if dropdown_open(flow) then
      local entries = browse_entries(flow)
      if name == "up" or name == "down" then
        local step = name == "down" and 1 or -1
        flow.browse_index = widgets.clamp((flow.browse_index or 1) + step, #entries)
        save(flow)
        return true
      elseif name == "backtab" then
        flow.browse = false
        flow.focus = "search"
        save(flow)
        return true
      elseif name == "enter" or name == "tab" then
        local entry = entries[widgets.clamp(flow.browse_index, #entries)]
        if entry then
          local dir = (browse().dir or "")
          local joined = (dir == "/" and "/" or (dir .. "/")) .. entry.name
          if name == "enter" then
            command("bookmark", { host = flow.host, repo = joined, action = "add" })
            textinput.clear(flow.input)
            flow.browse = false
            flow.focus = "search"
            await_new_row(flow, joined)
          else
            textinput.set(flow.input, joined .. "/")
            flow.browse_index = 1
          end
        end
        save(flow)
        ask(flow)
        return true
      end
    end

    if flow.focus == "search" then
      if
        (name == "left" and flow.wt_repo)
        or (name == "right" and flow.search.cursor == widgets.chars(flow.search.value or ""))
      then
        local entry = current_row(flow)
        local row = entry and entry.row
        if name == "left" and flow.wt_repo then
          flow.cursor = repo_picker.index_of(rows_for(flow), flow.wt_repo) or 1
          flow.wt_repo = nil
        elseif
          name == "right"
          and row
          and not row.is_parent
          and not row.is_worktree
          and (flow.host or "") == ""
        then
          flow.wt_repo = row.path
        end
        save(flow)
        ask(flow)
        return true
      elseif name == "tab" then
        enter_path_field(flow)
        save(flow)
        ask(flow)
        return true
      elseif name == "enter" then
        confirm_repos(flow)
        return true
      end
      if textinput.key(flow.search, key) then
        flow.cursor = 1
        save(flow)
        return true
      end
      return false
    end

    if flow.focus == "input" then
      if name == "tab" then
        local suggestion = suggestion_for(flow)
        if suggestion ~= "" then
          textinput.insert(flow.input, suggestion)
        else
          flow.browse = true
          flow.browse_index = 1
        end
        save(flow)
        ask(flow)
        return true
      elseif name == "backtab" then
        flow.browse = false
        flow.focus = "search"
        save(flow)
        return true
      elseif name == "enter" then
        local path = (flow.input.value or ""):match("^%s*(.-)%s*$")
        if names_missing_folder(flow) then
          flow.new_path = typed_target(flow)
          flow.folder_index = 1
          flow.step = "new_folder"
          flow.browse = false
        elseif other_users_home(flow) then
          flow.message = nil
        elseif flow.elsewhere and path:sub(1, 1) == "~" and not remote_home(flow) then
          flow.message = "asking " .. place_label(flow) .. " for its home…"
        elseif path ~= "" and not untouched(flow) then
          path = expand_remote_home(flow, path)
          command("bookmark", { host = flow.host, repo = path, action = "add" })
          textinput.clear(flow.input)
          enter_path_field(flow)
          flow.browse = false
          await_new_row(flow, path)
        end
        save(flow)
        ask(flow)
        return true
      end
      if untouched(flow) and (key.char == "/" or key.char == "~") then
        textinput.clear(flow.input)
      end
      if textinput.key(flow.input, key) then
        flow.browse_index = 1
        save(flow)
        ask(flow)
        return true
      end
      return false
    end
    return false
  end,

  on_click = function(hit)
    local flow = load()
    if not flow or not hit.id then
      return false
    end
    if flow.step == "repo" and hit.id == SEARCH_FIELD then
      if flow.focus ~= "search" then
        flow.browse = false
        flow.focus = "search"
        save(flow)
      end
      return true
    end
    if flow.step == "repo" and hit.id == PATH_FIELD then
      if flow.focus ~= "input" then
        enter_path_field(flow)
        save(flow)
        ask(flow)
      end
      return true
    end
    local browsed = flow.step == "repo" and hit.id:match("^" .. BROWSE_ROW .. "(.*)$")
    if browsed then
      local index = widgets.index_of(browse_entries(flow), browsed, function(entry)
        return entry.name
      end)
      if not index then
        return false
      end
      flow.browse_index = index
      save(flow)
      return true
    end
    if flow.step == "repo" then
      local index = widgets.index_of(rows_for(flow), hit.id, function(entry)
        return REPO_ROW .. entry.row.path
      end)
      if not index then
        return false
      end
      flow.cursor = index
      flow.focus = "search"
      save(flow)
      return true
    end
    local index = tonumber(hit.id)
    if not index then
      return false
    end
    if flow.step == "fleet" then
      flow.fleet_index = index
    elseif flow.step == "host" then
      flow.host_index = index
    elseif flow.step == "agent" then
      flow.agent_index = index
    elseif flow.step == "new_folder" then
      flow.folder_index = index
    elseif flow.step == "branch" then
      flow.branch_index = index
    end
    save(flow)
    return true
  end,
}
