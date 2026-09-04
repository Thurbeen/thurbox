-- The component layer: what a pane is made of, spelled once.
--
-- `lib/widgets.lua` is the primitive kit — measurement, windowing, a row of
-- text. This is the layer above it: the handful of shapes every pane in this
-- interface turns out to be, with the conventions that make them look like one
-- program rather than six.
--
-- The conventions, and why they are here rather than in each pane:
--
--   * **One focus border.** A panel's border and title come from
--     `lib/chrome`'s three levels. Three panes had grown three different
--     answers to "what does focused look like", and a fourth pane copied
--     whichever it was written next to.
--   * **One selection idiom.** A selected row is a full-width bar
--     (`selection_bg`/`selection_fg`), painted as the row's own `style`, never
--     a marker glyph eating two columns of every row. Hover is the same band
--     with the foreground left alone.
--   * **One empty state.** A blank line, the sentence centred, and — when
--     something is actually bound to it — the chord that fixes it, also
--     centred.
--   * **One footer.** Hints are resolved from the key registry, so a rebind
--     moves the hint with the key and a removed action takes its hint with it.
--
-- Everything here composes the four kernel node kinds. Nothing here is a node
-- kind, and nothing here is appearance the kernel decides: a pane still says
-- which glyph, which role and when to drop a trailing status. What it no
-- longer says is how a window is computed, where a selection bar comes from,
-- or what a border looks like when the pane has focus.

local chrome = require("lib.chrome")
local fuzzy = require("lib.fuzzy")
local hover = require("lib.hover")
local modal = require("lib.modal")
local scroll = require("lib.scroll")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

local ui = {}

-- ── Status ──────────────────────────────────────────────────────────────────

--- Glyph and colour for a session status, with `working` animated.
---
--- `theme.status` answers the static half; the spinner is the one status whose
--- glyph depends on the frame, and every pane that draws a status was choosing
--- between the two itself.
---@param name thurbox.Status
---@param elapsed number?
---@return { glyph: string, color: thurbox.Color? }
function ui.status(name, elapsed)
  local spec = theme.status(name)
  if name == "working" then
    return { glyph = theme.spinner_frame(elapsed), color = spec.color }
  end
  return spec
end

--- One status glyph per item, in render order — a panel's border strip.
---
--- `status_of` returns the status to draw, or nil for an item the strip skips
--- (work in flight has no status of its own yet).
---@param items table[]
---@param elapsed number?
---@param status_of fun(item: table): string?
---@return thurbox.Span[]
function ui.dots(items, elapsed, status_of)
  local runs = {}
  for _, item in ipairs(items) do
    local status = status_of(item)
    if status then
      local spec = ui.status(status, elapsed)
      runs[#runs + 1] = { text = spec.glyph, style = { fg = spec.color } }
    end
  end
  return runs
end

-- ── The key registry ────────────────────────────────────────────────────────

-- Memoized on the published table's identity: `thurbox.registry` is a gated
-- group, so seeing the same table object again means the same bindings — and a
-- footer resolves several actions per render, each of which would otherwise
-- walk every plugin's bindings.
local chord_cache = { src = nil, by_action = {} }

--- The chord bound to `action`, or nil when nothing is.
---
--- Read from the registry rather than written into a pane, because every chord
--- is rebindable and every action is removable: a hint naming a key that
--- resolves to nothing is worse than no hint at all.
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
  -- `false` marks "looked, nothing bound", so a miss is remembered too.
  chord_cache.by_action[action] = found or false
  return found
end

--- What the registry says an action does, for a hint that writes no label.
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

-- ── The cursor ──────────────────────────────────────────────────────────────

--- Read an item's identity. A falsy one means the item selects nothing — a
--- header, or a row for work that has no session yet.
local function identity(item, id)
  if item == nil then
    return nil
  end
  if type(id) == "function" then
    return id(item)
  end
  return item[id or "id"]
end

local Cursor = {}
Cursor.__index = Cursor

--- Clamp onto a selectable row, then publish where we landed.
---
--- Publishing is what lets another pane read the selection; remembering what
--- was published is what lets this one tell an outside write from its own echo.
function Cursor:_settle()
  local count = #self.items
  self.index = widgets.clamp(self.index, count)
  if count > 0 and not identity(self.items[self.index], self.key_of) then
    self.index = self:_step(self.index, 1)
  end
  state[self.prefix .. ".cursor"] = self.index
  local target = self:id()
  if self.steer then
    store[self.steer] = target
    state[self.prefix .. ".published"] = target
  end
end

--- The next index in `step`'s direction that selects something.
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

--- The selected item's identity, or nil when nothing is selected.
function Cursor:id()
  return identity(self.items[self.index], self.key_of) or nil
end

--- The selected item itself.
function Cursor:item()
  return self.items[self.index]
end

--- Move by `step` rows, skipping what selects nothing. Drops any follow: you
--- moved the cursor yourself, so it is no longer chasing a row.
function Cursor:move(step)
  self.index = self:_step(self.index, step)
  self:_stop_following()
  self:_settle()
  return self.index
end

--- Put the cursor at `index`.
function Cursor:select(index)
  self.index = index
  self:_stop_following()
  self:_settle()
  return self.index
end

--- Put the cursor on the row carrying `id`, if it is in the list.
function Cursor:select_by_id(id)
  for index, item in ipairs(self.items) do
    if identity(item, self.key_of) == id then
      return self:select(index)
    end
  end
  return nil
end

--- Chase `id` until a render lands on it.
---
--- Sticky rather than immediate because the row may not be here yet: a reorder
--- and a spawn both land a frame or two after the keystroke, and by then the
--- row that was under the cursor has moved or has only just appeared.
function Cursor:follow(id)
  ui.follow(self.prefix, id)
end

function Cursor:_stop_following()
  state[self.prefix .. ".follow"] = nil
end

--- The first visible row, 0-based — what `ui.list` scrolled to last frame.
function Cursor:offset()
  return state[self.prefix .. ".offset"] or 0
end

function Cursor:set_offset(offset)
  state[self.prefix .. ".offset"] = offset
end

--- Forget where the cursor was, for a list that opens fresh each time.
function Cursor:reset()
  ui.reset(self.prefix)
end

--- Forget where the cursor named `key` was — a list that opens fresh starts at
--- the top rather than wherever it was left the last time it was up.
---@param key string
function ui.reset(key)
  state[key .. ".cursor"] = nil
  state[key .. ".offset"] = nil
  state[key .. ".follow"] = nil
end

--- Ask the cursor named `key` to chase `id`, without holding one.
---
--- An event handler has no list to build a cursor over — the row it is being
--- told about may not be in the snapshot yet, which is the whole reason a
--- follow is sticky.
---@param key string
---@param id string?
function ui.follow(key, id)
  state[key .. ".follow"] = id
end

--- A list cursor that survives the list changing under it.
---
--- The state a pane with a list invariably grows — where the cursor is, where
--- the window is, which row it is chasing, and what it last told everyone else
--- — written once. Four panes had four spellings of it and none of them agreed
--- on what happens when the list is reordered under the selection.
---
--- Built fresh on every call, from `state`, so a render, a key handler and a
--- click handler all see the same cursor without passing one around.
---
--- opts:
---   id      — the field carrying an item's identity, or a function returning
---             it. An item with none selects nothing and is skipped.
---   steer   — a `store` key this cursor publishes its selection to, and reads
---             a *foreign* write of as a request to move. Another pane steering
---             the list is the reason this is not simply a write: publishing
---             every frame would otherwise undo the write a frame later, so a
---             value this cursor did not publish is followed instead.
---   request — a `store` key carrying a one-shot "go to this row" (a clicked OS
---             notification, `thurbox-cli session focus`). Consumed here,
---             because a plain `store` write would be overwritten by the next
---             publish.
---@param key string A name for this list's state, unique within the plugin.
---@param items table[]
---@param opts table?
function ui.cursor(key, items, opts)
  opts = opts or {}
  local self = setmetatable({
    prefix = key,
    items = items,
    key_of = opts.id,
    steer = opts.steer,
    index = state[key .. ".cursor"] or 1,
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
  local follow = state[key .. ".follow"]
  if follow then
    for index, item in ipairs(items) do
      if identity(item, self.key_of) == follow then
        self.index = index
        break
      end
    end
  end
  self:_settle()
  return self
end

-- ── The row builder ─────────────────────────────────────────────────────────

local Row = {}
Row.__index = Row

--- The style a span actually wears, after the row has had its say.
---
--- A span that named no style is left alone: it is structure — a separator, an
--- indent — and dimming or un-colouring it says nothing while making it
--- different from the same cells drawn by a pane that did not ask.
function Row:_tone(style)
  if style == nil or self.tone == nil then
    return style
  end
  return self.tone(style)
end

--- Append a run.
function Row:add(run, style)
  if run == nil or run == "" then
    return self
  end
  self.spans[#self.spans + 1] = { text = run, style = self:_tone(style) }
  self.used = self.used + widgets.len(run)
  return self
end

--- Append `n` blank columns.
function Row:gap(n)
  return self:add(string.rep(" ", math.max(0, n or 1)))
end

--- Append a run that is its own click target. A run carries `id`/`role` of its
--- own, so a chip inside a line needs no node with a hand-computed `len`.
function Row:button(label, style, role, id)
  if label == nil or label == "" then
    return self
  end
  self.spans[#self.spans + 1] = { text = label, style = self:_tone(style), role = role, id = id }
  self.used = self.used + widgets.len(label)
  return self
end

--- Append text with its search hits marked, or plain when there are none.
---
--- `hits` is what `fuzzy.match` returned. A marked run names its own colour, so
--- it survives a selection bar that supplies only what a span left unsaid —
--- which matters because previewing a result moves the cursor onto the very row
--- whose marks would otherwise disappear.
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

--- The row's trailing note — a status, an age — appended after a separator and
--- budgeted against what is left of the row.
---
--- Dropped rather than overflowed: a note that would arrive truncated to a
--- column or two says nothing and costs the columns something else could have
--- used. v1 drops it at the same threshold.
local SEPARATOR = "  "
local MIN_TRAILING = 4

--- `note` rather than `text`: `text` is the kernel's measurement global, and a
--- local of that name in a function that measures is a trap waiting for the next
--- edit.
function Row:trailing(note, style)
  if not note or note == "" then
    return self
  end
  -- A row with no width budgets nothing: it is being built for a rect nobody
  -- has resolved, and truncating against a guess is worse than not truncating.
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

--- The spans, ready for a `text` node.
function Row:spans_list()
  return self.spans
end

--- A span builder that knows how wide the row is.
---
--- The width is the whole point: every trailing note, every truncation and
--- every right-hand budget in this interface was computed by re-measuring the
--- spans built so far, in each pane, with its own idea of what the row's
--- columns were.
---
--- opts:
---   width — the row's columns (a list passes its own inner width). Omit it and
---           `:trailing` budgets nothing, which is what a row built for a rect
---           nobody has resolved wants
---   tone  — a function every span's style passes through, which is how a row
---           says "this whole row is dimmed" or "the bar underneath speaks for
---           every colour" without each caller branching
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

--- `── label ───────`, muted, full bleed — the rule that heads a group.
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

-- ── The empty state ─────────────────────────────────────────────────────────

local function centred(sentence, width)
  local pad = math.max(0, math.floor((width - widgets.len(sentence)) / 2))
  return { { text = string.rep(" ", pad) .. sentence, style = { fg = theme.muted } } }
end

--- The one empty state: a blank line, the sentence, and the way out.
---
--- Returns the LINES rather than a node, so a modal can size itself against
--- them — a float is measured in cells and an empty state that grew a line
--- would otherwise clip its own footer.
---
--- `hint_action` is looked up in the registry and the hint is dropped when
--- nothing is bound: an empty state advertising a chord that does nothing is
--- the failure this whole indirection exists to prevent.
---
--- opts: title, width, hint (a format string taking the chord), hint_action
---@param opts table
---@return thurbox.Span[][]
function ui.empty(opts)
  local width = opts.width or 0
  local lines = { {}, centred(opts.title or "", width) }
  local chord = opts.hint_action and ui.chord(opts.hint_action)
  if chord and opts.hint then
    lines[#lines + 1] = centred(string.format(opts.hint, chord), width)
  end
  return lines
end

-- ── The list ────────────────────────────────────────────────────────────────

--- An overflow marker: a row of its own, never drawn over one.
local function marker(label)
  return { type = "text", len = 1, text = theme.dim(label), role = "overflow" }
end

--- The first visible item, and how many rows are hidden at each end.
local function window_of(heights, offset, selected, height)
  local first, visible = scroll.window_variable(heights, offset, selected, height)
  return first, first - 1, #heights - (first - 1 + visible)
end

--- The window with room kept for however many overflow markers it needs.
---
--- A marker is a row, so it costs a line the rows would otherwise have had —
--- and giving up that line can push a further row out of view, which calls for
--- the second marker too. Hence the escalation rather than one subtraction.
---
--- A list with no line to spare shows rows and no markers: a strip made
--- entirely of "N more" says nothing.
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

--- The selection bar and the hover band.
---
--- Built per row rather than hoisted, because a theme change must be picked up
--- on the next frame and a role reads through the theme's own memo — and at
--- most two rows on screen wear one.
local function selected_style()
  return {
    bg = theme.role("selection_bg"),
    fg = theme.role("selection_fg"),
    bold = true,
  }
end

--- Only the BACKGROUND is tinted, so a status dot and a branch colour survive
--- being pointed at. A button gets the stronger accent fill instead; a row is
--- not a button.
local function hover_style()
  return { bg = theme.role("selection_bg") }
end

--- A scrolling list of rows, with the selection bar and the window written
--- once.
---
--- What a pane still decides: what a row says (`row`), what heads a group
--- (`header`), and what "nothing here" reads as. What it no longer decides:
--- where the window is, how the selection looks, whether a hidden row is
--- announced, or how any of that is spelled.
---
--- The returned box carries `above` and `below` — the counts of rows hidden at
--- each end. `ui.panel` reads them off the node, which is what puts a `▲ N` on
--- the border instead of costing a content row. (An extra key on a node table
--- is how a plugin carries its own bookkeeping; the kernel ignores it.)
---
--- opts:
---   items       — the model rows, in render order
---   cursor      — a `ui.cursor`, or a plain index
---   width       — the row's columns, for the empty state's centring
---   height      — the rows available
---   row(item, selected) → spans
---   header(item) → spans | spans[] | nil, drawn above the row and never
---                  selected: a group heading glued to its group's first row,
---                  so clicking the heading selects that row and the window
---                  arithmetic counts the pair as one item
---   id_of(item) → the row's identity, for hover, clicks and decoration
---                 (defaults to the cursor's)
---   class_of(item) → an extra class on the row
---   empty       — the lines `ui.empty` returned, or a string to centre
---   on_overflow — "rows" (markers take a line each) or "border" (the counts
---                 ride the frame, and no line is spent on them)
---   pad         — fill the remaining lines with blanks, so the box is exactly
---                 `height` rows however few items there are
---   len, fill   — the returned box's own size
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
    -- Headers are built in the pass that measures the list: they decide an
    -- item's height, and building them again to draw them would be the second
    -- source of truth this module exists to remove.
    local heights, headers = {}, {}
    for index, item in ipairs(items) do
      local head = opts.header and opts.header(item) or nil
      if head and #head == 0 then
        head = nil
      elseif head and head[1].text ~= nil then
        -- One span list, not a list of them: a SPAN is what carries `text`.
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
      -- A header may be the last thing that fits: it belongs to the row below
      -- it, and drawing the pair or nothing would leave a blank line where the
      -- reader can see there is more list.
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
        -- The bar is the row's own `style`: the kernel paints it across the
        -- row's rect before the spans go on top, so it reaches the border with
        -- no spacer span and no style merged into every span by hand. It is
        -- the row's and not the list's, so it never bleeds onto a header glued
        -- above a group's first row.
        local style
        if is_selected then
          style = selected_style()
        elseif id and hover.id(id) then
          style = hover_style()
        end
        children[#children + 1] = {
          type = "text",
          len = 1,
          text = { opts.row(item, is_selected) },
          style = style,
          id = id or nil,
          class = table.concat(classes, " "),
          -- Decoration finds rows by this role, and only the content is inside
          -- it — so a highlight can never repaint the border.
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

-- ── The panel ───────────────────────────────────────────────────────────────

--- `glyph N ` laid over the tail of `strip`.
---
--- v1 LAYERS the two: the count is painted onto border cells the dot strip
--- already occupies, so it covers the last dots rather than pushing them left.
--- One right-aligned run list says the same thing, and the dots it would have
--- covered are dropped here instead of overwritten there.
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

--- A framed pane, in the one focus convention this interface has.
---
--- Focus is communicated by COLOUR — a brighter border and a title badge —
--- never by a marker glyph, which is a rule the agent pane states and one
--- panel builder used to contradict.
---
--- The overlays paint onto the border cells the block already drew, so a status
--- strip, a scroll count or a scrollbar costs no content row and no content
--- column.
---
--- A `body` built by `ui.list` with `on_overflow = "border"` carries its own
--- hidden-row counts, and they are laid over the top-right and bottom-right
--- overlays here rather than by the pane.
---
--- opts: title, focused, level, body, overlay_left, overlay_right,
---       right_column, border, title_align
---@param opts table
---@return thurbox.BoxNode
function ui.panel(opts)
  local level = opts.level or (opts.focused and "focused" or "active")
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
      title = { { text = " " .. (opts.title or "") .. " ", style = chrome.title_style(level) } },
      title_align = opts.title_align,
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

-- ── The float ───────────────────────────────────────────────────────────────

--- A modal float, sized from what is in it.
---
--- Every float in this interface had hand-summed its own height, and each sum
--- had to be revisited whenever a row was added to its body — which is how one
--- of them came to clip its own footer. `rows` is still accepted for a float
--- whose body is not a list of fixed-height children.
---
--- opts: title, cols, rows, children, crumbs, border
---@param opts table
---@return thurbox.BoxNode
function ui.modal(opts)
  local rows = opts.rows
  if not rows then
    -- The borders, plus every child that declared a height. A child that did
    -- not is the caller's business to size, which is what `rows` is for.
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

--- The footer strip: key hints on the left, the confirm and dismiss pills on
--- the right.
---
--- Hints are resolved from the key registry rather than written out, so a
--- rebind moves the hint with the key and an action nobody bound shows no hint
--- at all. An entry is one of:
---
---   "plugin.action"                  — the chord and the description the
---                                      binding itself declares
---   { "plugin.action", "label" }     — that chord, this label
---   { { "a", "b" }, "label" }        — both chords, joined with `/`
---   { key = "esc", label = "close" } — a chord the registry does not carry
---                                      (`esc` and `enter` belong to every
---                                      modal and are declared by none)
---
--- opts: actions, primary, cancel, key, style
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
