local chrome = require("lib.chrome")
local fuzzy = require("lib.fuzzy")
local hover = require("lib.hover")
local modal = require("lib.modal")
local scroll = require("lib.scroll")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

local ui = {}

local SCREEN_GLYPHS = { blocked = "◇" }

---@param name thurbox.Status
---@param elapsed number?
---@param printing boolean?
---@param source thurbox.StatusSource?
---@return { glyph: string, color: thurbox.Color?, dim: boolean? }
function ui.status(name, elapsed, printing, source)
  if source == "none" then
    return { glyph = "?", color = theme.status("unreported").color }
  end
  local spec = theme.status(name)
  if name == "working" or (name == "running" and printing) then
    spec = { glyph = theme.spinner_frame(elapsed), color = spec.color }
  end
  if source == "screen" then
    return { glyph = SCREEN_GLYPHS[name] or spec.glyph, color = spec.color, dim = true }
  end
  return { glyph = spec.glyph, color = spec.color }
end

---@param session_id string?
---@return boolean
function ui.printing(session_id)
  if not session_id then
    return false
  end
  local set = thurbox and thurbox.printing
  return set ~= nil and set[session_id] == true
end

---@param items table[]
---@param elapsed number?
---@param status_of fun(item: table): string?
---@param id_of (fun(item: table): string?)?
---@return thurbox.Span[]
function ui.dots(items, elapsed, status_of, id_of)
  local runs = {}
  for _, item in ipairs(items) do
    local status = status_of(item)
    if status then
      local printing = id_of ~= nil and ui.printing(id_of(item)) or false
      local spec = ui.status(status, elapsed, printing)
      runs[#runs + 1] = { text = spec.glyph, style = { fg = spec.color } }
    end
  end
  return runs
end

local chord_cache = { src = nil, by_action = {} }

---@param action string
---@return string?
function ui.chord(action)
  local registry = thurbox and thurbox.registry
  if not rawequal(registry, chord_cache.src) then
    chord_cache.src = registry
    chord_cache.by_action = {}
  end
  local cached = chord_cache.by_action[action]
  if cached ~= nil then
    return cached or nil
  end
  local found
  for _, binding in ipairs((registry and registry.keys) or {}) do
    if binding.action == action and binding.key then
      found = binding.key
      break
    end
  end
  chord_cache.by_action[action] = found or false
  return found
end

---@param action string
---@return string?
function ui.describe(action)
  for _, binding in ipairs((thurbox and thurbox.registry and thurbox.registry.keys) or {}) do
    if binding.action == action and binding.desc and binding.desc ~= "" then
      return binding.desc
    end
  end
  return nil
end

local function identity(item, id)
  if item == nil then
    return nil
  end
  if type(id) == "function" then
    return id(item)
  end
  return item[id or "id"]
end

local function row_of(items, key_of, wanted)
  if wanted == nil then
    return nil
  end
  for index, item in ipairs(items) do
    if identity(item, key_of) == wanted then
      return index
    end
  end
  return nil
end

local Cursor = {}
Cursor.__index = Cursor

function Cursor:_settle()
  local count = #self.items
  self.index = widgets.clamp(self.index, count)
  if count > 0 and not identity(self.items[self.index], self.key_of) then
    self.index = self:_step(self.index, 1)
  end
  state[self.prefix .. ".cursor"] = self.index
  local target = self:id()
  state[self.prefix .. ".selected"] = target
  if self.steer then
    local published = target
    if self.publish then
      published = self.publish(self:item())
    end
    store[self.steer] = published
    state[self.prefix .. ".published"] = published
  end
end

function Cursor:_step(from, step)
  local count = #self.items
  if count == 0 then
    return 1
  end
  local at = from
  for _ = 1, count do
    at = (at - 1 + step) % count + 1
    if identity(self.items[at], self.key_of) then
      return at
    end
  end
  return from
end

function Cursor:id()
  return identity(self.items[self.index], self.key_of) or nil
end

function Cursor:item()
  return self.items[self.index]
end

function Cursor:move(step)
  self.index = self:_step(self.index, step)
  self:_stop_following()
  self:_settle()
  return self.index
end

function Cursor:select(index)
  self.index = index
  self:_stop_following()
  self:_settle()
  return self.index
end

function Cursor:select_by_id(id)
  local at = row_of(self.items, self.key_of, id)
  if at then
    return self:select(at)
  end
  return nil
end

function Cursor:follow(id)
  ui.follow(self.prefix, id)
end

function Cursor:_stop_following()
  state[self.prefix .. ".follow"] = nil
end

function Cursor:offset()
  return state[self.prefix .. ".offset"] or 0
end

function Cursor:set_offset(offset)
  state[self.prefix .. ".offset"] = offset
end

function Cursor:reset()
  ui.reset(self.prefix)
end

---@param key string
function ui.reset(key)
  state[key .. ".cursor"] = nil
  state[key .. ".selected"] = nil
  state[key .. ".offset"] = nil
  state[key .. ".follow"] = nil
end

---@param key string
---@param id string?
function ui.follow(key, id)
  state[key .. ".follow"] = id
end

---@param key string A name for this list's state, unique within the mod.
---@param items table[]
---@param opts table?
function ui.cursor(key, items, opts)
  opts = opts or {}
  local initial = opts.initial
  if type(initial) == "function" then
    initial = initial(items)
  end
  local self = setmetatable({
    prefix = key,
    items = items,
    key_of = opts.id,
    steer = opts.steer,
    publish = opts.publish,
    index = state[key .. ".cursor"] or initial or 1,
  }, Cursor)

  if opts.request and store[opts.request] then
    state[key .. ".follow"] = store[opts.request]
    store[opts.request] = nil
  end
  if self.steer then
    local steered = store[self.steer]
    if steered and steered ~= state[key .. ".published"] then
      state[key .. ".follow"] = steered
    end
  end
  local at = row_of(items, self.key_of, state[key .. ".follow"])
    or row_of(items, self.key_of, state[key .. ".selected"])
  if at then
    self.index = at
  end
  self:_settle()
  return self
end

local Row = {}
Row.__index = Row

function Row:_tone(style)
  if style == nil or self.tone == nil then
    return style
  end
  return self.tone(style)
end

function Row:add(run, style)
  if run == nil or run == "" then
    return self
  end
  self.spans[#self.spans + 1] = { text = run, style = self:_tone(style) }
  self.used = self.used + widgets.len(run)
  return self
end

function Row:gap(n)
  return self:add(string.rep(" ", math.max(0, n or 1)))
end

function Row:button(label, style, role, id)
  if label == nil or label == "" then
    return self
  end
  if hover.role(role) or hover.id(id) then
    style = hover.button_style()
  end
  self.spans[#self.spans + 1] = { text = label, style = self:_tone(style), role = role, id = id }
  self.used = self.used + widgets.len(label)
  return self
end

function Row:match(subject, hits, style, hit_style)
  if not hits then
    return self:add(subject, style)
  end
  for _, span in ipairs(fuzzy.spans(subject, hits, self:_tone(style), hit_style)) do
    self.spans[#self.spans + 1] = span
  end
  self.used = self.used + widgets.len(subject)
  return self
end

local SEPARATOR = "  "
local MIN_TRAILING = 4

function Row:trailing(note, style)
  if not note or note == "" then
    return self
  end
  if not self.width then
    return self:add(SEPARATOR):add(note, style)
  end
  local avail = math.max(0, self.width - self.used - widgets.len(SEPARATOR))
  if avail < MIN_TRAILING then
    return self
  end
  self:add(SEPARATOR)
  return self:add(widgets.truncate_hard(note, avail), style)
end

function Row:spans_list()
  return self.spans
end

---@param opts table?
function ui.row(opts)
  opts = opts or {}
  return setmetatable({
    spans = {},
    used = 0,
    width = opts.width,
    tone = opts.tone,
  }, Row)
end

---@param label string
---@param width integer
---@return thurbox.Span[]
function ui.rule(label, width)
  local line = "── " .. label .. " "
  local used = widgets.len(line)
  if width > used then
    line = line .. string.rep("─", width - used)
  end
  return { { text = line, style = { fg = theme.muted } } }
end

local function centred(sentence, width, colour)
  local pad = math.max(0, math.floor((width - widgets.len(sentence)) / 2))
  return {
    { text = string.rep(" ", pad) .. sentence, style = { fg = colour or theme.muted } },
  }
end

---@param opts table
---@return thurbox.Span[][]
function ui.empty(opts)
  local width = opts.width or 0
  local lines = { {}, centred(opts.title or "", width) }
  local chord = opts.hint_action and ui.chord(opts.hint_action)
  if chord and opts.hint then
    lines[#lines + 1] = centred(string.format(opts.hint, chord), width)
  end
  if opts.note then
    lines[#lines + 1] = {}
    local note = type(opts.note) == "table" and opts.note or { opts.note }
    for _, sentence in ipairs(note) do
      lines[#lines + 1] = centred(sentence, width, opts.note_colour)
    end
  end
  return lines
end

local function marker(label)
  return { type = "text", len = 1, text = theme.dim(label), role = "overflow" }
end

local function window_of(heights, offset, selected, height)
  local first, visible = scroll.window_variable(heights, offset, selected, height)
  return first, first - 1, #heights - (first - 1 + visible)
end

local function marked_window(heights, offset, selected, height)
  for markers = 0, 2 do
    if height - markers < 1 then
      break
    end
    local first, above, below = window_of(heights, offset, selected, height - markers)
    if (above > 0 and 1 or 0) + (below > 0 and 1 or 0) <= markers then
      return first, above, below
    end
  end
  return (window_of(heights, offset, selected, height)), 0, 0
end

local function selected_style()
  return {
    bg = theme.role("selection_bg"),
    fg = theme.role("selection_fg"),
    bold = true,
  }
end

local function hover_style()
  return { bg = theme.role("selection_bg") }
end

---@param opts table
function ui.list(opts)
  local items = opts.items or {}
  local width = opts.width or 0
  local cursor = opts.cursor
  local tracked = type(cursor) == "table"
  local selected = tracked and cursor.index or (cursor or 1)
  local height = opts.height or #items
  local children, above, below = {}, 0, 0

  local function line(spans)
    children[#children + 1] = { type = "text", len = 1, text = { spans } }
  end

  if #items == 0 then
    local lines = opts.empty
    if type(lines) == "string" then
      lines = { centred(lines, width) }
    end
    for _, spans in ipairs(lines or {}) do
      line(spans)
    end
  else
    local heights, headers = {}, {}
    for index, item in ipairs(items) do
      local head = opts.header and opts.header(item) or nil
      if head and #head == 0 then
        head = nil
      elseif head and head[1].text ~= nil then
        head = { head }
      end
      headers[index] = head
      heights[index] = 1 + (head and #head or 0)
    end

    local offset = tracked and cursor:offset() or 0
    local first
    if opts.on_overflow == "rows" then
      first, above, below = marked_window(heights, offset, selected, height)
    else
      first, above, below = window_of(heights, offset, selected, height)
    end
    if tracked then
      cursor:set_offset(first - 1)
    end

    local markers = ((opts.on_overflow == "rows" and above > 0) and 1 or 0)
      + ((opts.on_overflow == "rows" and below > 0) and 1 or 0)
    local budget = height - markers
    if opts.on_overflow == "rows" and above > 0 then
      children[#children + 1] = marker("  ↑ " .. above .. " more")
    end

    local drawn = 0
    for index = first, #items do
      if drawn >= budget then
        break
      end
      for _, head in ipairs(headers[index] or {}) do
        if drawn >= budget then
          break
        end
        line(head)
        drawn = drawn + 1
      end
      if drawn < budget then
        local item = items[index]
        local is_selected = index == selected
        local id = (opts.id_of and opts.id_of(item))
          or (tracked and identity(item, cursor.key_of))
          or nil
        local classes = { "row" }
        local extra = opts.class_of and opts.class_of(item)
        if extra then
          classes[#classes + 1] = extra
        end
        if is_selected then
          classes[#classes + 1] = "selected"
        end
        local style
        if is_selected then
          style = selected_style()
        elseif id and hover.id(id) then
          style = hover_style()
        elseif opts.style_of then
          style = opts.style_of(item)
        end
        children[#children + 1] = {
          type = "text",
          len = 1,
          text = { opts.row(item, is_selected) },
          style = style,
          id = id or nil,
          class = table.concat(classes, " "),
          role = id and "row" or nil,
        }
        drawn = drawn + 1
      end
    end

    if opts.on_overflow == "rows" and below > 0 then
      children[#children + 1] = marker("  ↓ " .. below .. " more")
    end
  end

  if opts.pad then
    while #children < height do
      line({})
    end
  end

  return {
    type = "box",
    len = opts.len,
    fill = opts.fill,
    children = children,
    above = above,
    below = below,
  }
end

local function with_count(strip, glyph, count)
  if count <= 0 then
    return strip
  end
  local text = glyph .. " " .. count .. " "
  local keep = 0
  for _, run in ipairs(strip) do
    keep = keep + widgets.len(run.text)
  end
  keep = keep - widgets.len(text)
  local runs, used = {}, 0
  for _, run in ipairs(strip) do
    local run_width = widgets.len(run.text)
    if used + run_width > keep then
      break
    end
    used = used + run_width
    runs[#runs + 1] = run
  end
  runs[#runs + 1] = { text = text, style = { fg = theme.muted } }
  return runs
end

---@param opts table
---@return thurbox.BoxNode
function ui.panel(opts)
  local level = opts.level or chrome.level(opts.focused)
  local body = opts.body
  local children = body
  if body and body.type then
    children = { body }
  end

  local top_right = opts.overlay_right
  local bottom_right
  if opts.body and type(opts.body.above) == "number" then
    top_right = with_count(top_right or {}, "▲", opts.body.above)
    bottom_right = with_count({}, "▼", opts.body.below)
  end

  return {
    type = "box",
    children = children,
    frame = {
      title = {
        { text = chrome.label(opts.title or "", level), style = chrome.title_style(level) },
      },
      title_align = opts.title_align,
      border_type = chrome.border_type(level),
      border_style = opts.border or chrome.border_style(level),
      overlay = {
        top_left = opts.overlay_left,
        top_right = top_right,
        bottom_right = bottom_right,
        right_column = opts.right_column,
      },
    },
  }
end

---@param opts table
---@return thurbox.BoxNode
function ui.modal(opts)
  local rows = opts.rows
  if not rows then
    rows = 2
    for _, child in ipairs(opts.children or {}) do
      rows = rows + (child.len or 0)
    end
  end
  return modal.frame(opts.title, {
    cols = opts.cols,
    rows = rows,
    crumbs = opts.crumbs,
    border = opts.border,
    children = opts.children,
  })
end

---@param opts table
---@return thurbox.BoxNode
function ui.footer(opts)
  local hints = {}
  for _, entry in ipairs(opts.actions or {}) do
    local label, chord
    if type(entry) == "string" then
      chord, label = ui.chord(entry), ui.describe(entry)
    elseif entry.key then
      chord, label = entry.key, entry.label
    else
      label = entry[2]
      local actions = type(entry[1]) == "table" and entry[1] or { entry[1] }
      local chords = {}
      for _, action in ipairs(actions) do
        local found = ui.chord(action)
        if found then
          chords[#chords + 1] = found
        end
      end
      chord = #chords > 0 and table.concat(chords, "/") or nil
    end
    if chord and label then
      hints[#hints + 1] = { chord, label }
    end
  end
  return modal.footer(hints, opts.primary, {
    cancel = opts.cancel,
    key = opts.key,
    style = opts.style,
  })
end

return ui
