-- Global search.
--
-- v1's is one strip that searches every scope at once and highlights the matches
-- **in the panes themselves** rather than reprinting them here. That is the
-- design decision worth preserving: results you can see in place are results you
-- can judge without leaving the list you were already reading — which is also
-- why this is a strip and not a float. A float would cover the rows it is
-- pointing at.
--
-- Three things follow from it, and each one is a gap this file closes:
--
--   * The pane holding a row draws its own highlight, reading the query from
--     `store` and matching with the same `lib.fuzzy` this uses. Search decides
--     what matches; the pane decides how a match looks in its own row.
--   * Moving the selection PREVIEWS: the owning pane's cursor moves to the
--     result while focus stays here, so you see the session before committing to
--     it. `esc` puts back what you were looking at; `enter` keeps the jump.
--   * Matching is subsequence, not substring — `fb` finds `fix-branch`.
--
-- Sessions are searched by their metadata AND by what their terminal is
-- showing, which is the half that finds a session by the error in it. The screen
-- text is a read the kernel serves only while this pane asks for it — every
-- agent's screen on every frame is not a thing to publish speculatively — so the
-- query is left in `store` under `want_content` and the answer arrives as
-- `thurbox.content`, one frame later. Debounced, because narrowing a query is a
-- keystroke at a time and each one would otherwise rescan every screen.
--
-- Scope. v1 searches sessions, tasks, automations and files. Only the session
-- list currently exists as a pane, so only sessions are searched: a result has
-- to be able to *land* somewhere, and a scope whose pane was removed would
-- produce rows that go nowhere when you press enter. The result shape below
-- carries the pane it belongs to, so a returning pane is a scope added here and
-- nothing else changed.

local fuzzy = require("lib.fuzzy")
local panels = require("lib.panels")
local textinput = require("lib.textinput")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

local NAME = "search"
local OPEN = "search.open"
local NEXT, PREVIOUS = "search.next", "search.previous"
local ACTIVATE, CANCEL = "search.activate", "search.cancel"

--- The live query, in `store` rather than in `state`.
---
--- It has to be readable from OUTSIDE this plugin: the session list dims and
--- highlights its own rows against it, and `store` is the documented bus between
--- panes. Cleared on close, so a pane can treat "there is a query" and "search is
--- open" as the same question.
local QUERY = "search.query"

--- Where the kernel is asked for terminal text, and where it answers.
---
--- The value is the query itself, so "is anything asking" and "is there a query"
--- are one question rather than two that can disagree.
local WANT_CONTENT = "want_content"

--- How long a query must stand still before screens are scanned. v1 waits the
--- same 150ms (`CONTENT_DEBOUNCE_MS`).
local CONTENT_DEBOUNCE = 0.15

--- Longest snippet shown for a content hit, in characters. v1's cap.
local SNIPPET_CHARS = 120

-- ── State ───────────────────────────────────────────────────────────────────
--
-- Read whole, written whole: `state` hands back a fresh table on every read, so
-- a field mutated in place is silently lost.

local function load()
  return {
    field = state.field or textinput.new(""),
    cursor = state.cursor or 1,
    -- What the interface looked like when search opened, so cancelling can put
    -- it back. v1 captures the same three things in its `SearchSnapshot`.
    snapshot = state.snapshot,
    -- The query the debounce is timing, and when it was last seen to change.
    -- Measured in `ctx.elapsed` seconds: a plugin has no clock of its own, and
    -- the snapshot's instant only moves when the snapshot does.
    pending = state.pending,
    pending_at = state.pending_at,
    -- The result this pane last pointed the list at. Kept because the result set
    -- can change without a keystroke: terminal text arrives a frame after the
    -- query settles, and a preview that only follows the arrows would stay on
    -- whatever was under the cursor before the content hits appeared.
    previewed = state.previewed,
  }
end

local function save(search)
  state.field = search.field
  state.cursor = search.cursor
  state.snapshot = search.snapshot
  state.pending = search.pending
  state.pending_at = search.pending_at
  state.previewed = search.previewed
end

local function query()
  return store[QUERY] or ""
end

-- ── Results ─────────────────────────────────────────────────────────────────

--- The fields of a session worth matching, in the order they are tried.
---
--- Name first so a row highlights its own name in the common case; the rest let
--- you find a session by what it is working on rather than what it was called.
local function fields_of(session)
  return {
    { name = "name", text = session.name or "" },
    { name = "agent", text = session.agent or "" },
    { name = "branch", text = session.branch or "" },
    { name = "repo", text = session.repo or "" },
  }
end

--- The terminal text the kernel has served, if any.
local function screens()
  return (thurbox and thurbox.content) or {}
end

--- The first line of `text` containing `needle`, trimmed and capped.
---
--- Plain substring, not the subsequence matching used on metadata: a whole
--- screen of text contains almost any short subsequence, so fuzzy here would
--- match everything and mean nothing. v1 draws the same line — `fuzzy_match` for
--- the metadata, `contains` for the buffer.
local function line_containing(text, needle)
  local wanted = needle:lower()
  for line in (text or ""):gmatch("[^\n]+") do
    if line:lower():find(wanted, 1, true) then
      local trimmed = line:match("^%s*(.-)%s*$")
      if trimmed ~= "" then
        return widgets.truncate(trimmed, SNIPPET_CHARS)
      end
    end
  end
  return nil
end

--- Per-session memo of the screen scan above.
---
--- The kernel already gates its side of the content pipeline on output
--- generation; this is the Lua half. `line_containing` walks the whole grid
--- text and lowercases every line, and it ran per session per frame while
--- typing. Keyed on the content string and the query — string equality is a
--- length check and a memcmp, far cheaper than the scan it prevents.
local scan_cache = {}

local function screen_hit(id, text, needle)
  if not text or text == "" then
    return nil
  end
  local cached = scan_cache[id]
  if cached and cached.text == text and cached.query == needle then
    return cached.hit or nil
  end
  local hit = line_containing(text, needle)
  -- `false` remembers "scanned, no hit"; nil would re-scan every frame.
  scan_cache[id] = { text = text, query = needle, hit = hit or false }
  return hit
end

--- Every result for the current query, grouped by scope.
---
--- An empty query yields every session, which is what makes the strip useful the
--- moment it opens rather than only once you have typed something.
local function results()
  local text = query()
  local needle = fuzzy.compile(text)
  local content = screens()
  local rows = {}
  local live = {}
  for _, session in ipairs(thurbox.sessions or {}) do
    live[session.id] = true
    local field, positions, matched = fuzzy.first(needle, fields_of(session))
    if field then
      rows[#rows + 1] = {
        scope = "sessions",
        pane = "sessions",
        id = session.id,
        label = session.name or session.id,
        -- Only the name is highlighted in place, so a hit anywhere else has to
        -- say so — otherwise the row looks like it matched nothing.
        positions = field == "name" and positions or nil,
        snippet = field ~= "name" and (field .. ": " .. matched) or nil,
      }
    elseif text ~= "" then
      -- Nothing in the metadata matched, so the screen is the last place to
      -- look. Only for sessions that did NOT already match, as v1 does: a
      -- session listed twice is one result pretending to be two.
      local hit = screen_hit(session.id, content[session.id], text)
      if hit then
        rows[#rows + 1] = {
          scope = "sessions",
          pane = "sessions",
          id = session.id,
          label = session.name or session.id,
          snippet = hit,
        }
      end
    end
  end
  -- Scans for sessions that no longer exist would otherwise live forever.
  for id in pairs(scan_cache) do
    if not live[id] then
      scan_cache[id] = nil
    end
  end
  return rows
end

--- Scope counts, for the per-scope summary line.
local function counts(rows)
  local totals, order = {}, {}
  for _, row in ipairs(rows) do
    if not totals[row.scope] then
      order[#order + 1] = row.scope
    end
    totals[row.scope] = (totals[row.scope] or 0) + 1
  end
  return totals, order
end

-- ── Preview and jump ────────────────────────────────────────────────────────

--- Point the owning pane's cursor at a result, without leaving the strip.
---
--- This is the whole of "live preview": the session list follows a `store`
--- selection it did not write itself, so writing one moves its cursor and the
--- row scrolls into view while focus stays here.
local function preview(row)
  if not row then
    return
  end
  panels.show(row.pane)
  store.selected = row.id
end

--- Put back what was on screen before search opened.
---
--- Cancelling has to be a real undo, not just a close: previewing has already
--- moved a cursor, and leaving it where the last previewed result put it would
--- make `esc` a way to change the selection by accident.
local function restore(snapshot)
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

--- Ask the kernel for terminal text once the query has stood still.
---
--- Called from render because that is where the clock is: `ctx.elapsed` is the
--- only monotonic reading a plugin gets, and `on_key` has none. A query still
--- settling asks for nothing, so the screens are scanned once per pause rather
--- than once per keystroke.
local function want_content(search, elapsed)
  local text = query()
  if text == "" then
    store[WANT_CONTENT] = nil
    search.pending, search.pending_at = nil, nil
    return
  end
  if search.pending ~= text then
    search.pending, search.pending_at = text, elapsed
    return
  end
  if elapsed - (search.pending_at or elapsed) >= CONTENT_DEBOUNCE then
    store[WANT_CONTENT] = text
  end
end

local function close(search, keep)
  if not keep then
    restore(search.snapshot)
  end
  search.snapshot = nil
  textinput.clear(search.field)
  search.cursor = 1
  search.pending, search.pending_at = nil, nil
  search.previewed = nil
  save(search)
  store[QUERY] = nil
  -- Stop the kernel reading screens the moment nothing is looking at them.
  store[WANT_CONTENT] = nil
  panels.hide(NAME)
end

-- ── Rendering ───────────────────────────────────────────────────────────────

local function query_row(search, rows)
  local total = #rows
  local position = total > 0 and math.min(search.cursor, total) or 0
  local tail = total > 0 and ("[" .. position .. "/" .. total .. "]") or "no matches"
  local label = " Search "
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
        placeholder = "type to search sessions",
        -- The strip is only drawn while searching, and while it is the query is
        -- what the keyboard is aimed at.
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

--- The per-scope summary v1 shows beside the query: `sessions 3`.
local function summary(rows)
  local totals, order = counts(rows)
  local parts = {}
  for _, scope in ipairs(order) do
    parts[#parts + 1] = scope .. " " .. totals[scope]
  end
  if #parts == 0 then
    return nil
  end
  return table.concat(parts, " · ")
end

--- One result row: the label with its matched characters lit, and the snippet
--- that explains a match you cannot see in the label.
---
--- No selection marker of its own — `widgets.list` draws the `▸`, and a second
--- one here would print it twice.
local function result_line(row, selected, width)
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

--- Results as lines, with a header per scope, plus the LINE the cursor's result
--- sits on.
---
--- The two spaces differ by one header per scope, and only one of them may be
--- the cursor: selection is in RESULT space, because a header is not a thing you
--- can pick — the theme picker had to be fixed for exactly this. So the mapping
--- is returned rather than recomputed by the caller.
local function result_rows(rows, cursor, width, height)
  -- Line numbers are assigned arithmetically first, then spans are built only
  -- for the visible window: every result used to be fully rendered — fuzzy
  -- spans, truncation and all — for a strip that shows a handful. The window
  -- is the same `widgets.window` call the list itself makes over the same
  -- (count, height, selected), so the two agree on which lines are on screen;
  -- an off-window entry is a placeholder whose spans the list never reads.
  local entries, selected_line = {}, 1
  local scope = nil
  for index, row in ipairs(rows) do
    if row.scope ~= scope then
      scope = row.scope
      entries[#entries + 1] = { header = scope }
    end
    entries[#entries + 1] = { row = row, index = index }
    if index == cursor then
      selected_line = #entries
    end
  end

  local first, last = widgets.window(#entries, height, selected_line)
  local lines = {}
  for at, entry in ipairs(entries) do
    if at < first or at > last then
      lines[at] = {
        spans = {},
        id = entry.header and ("header:" .. entry.header) or entry.row.id,
      }
    elseif entry.header then
      lines[at] = {
        spans = { { text = entry.header, style = { fg = theme.muted } } },
        -- Not addressable: a click on a header must not resolve to a result.
        id = "header:" .. entry.header,
      }
    else
      lines[at] = result_line(entry.row, entry.index == cursor, width)
    end
  end
  return lines, selected_line
end

return {
  name = NAME,
  slot = NAME,
  order = 65,
  -- Deliberately NOT `pure`. This render writes to `store` — the debounced
  -- `want_content` request and the pane-count hint — so a frame that skipped it
  -- would skip those writes too, and the search would stop asking for the
  -- terminal text it matches against.
  -- Focusable, and focused while it is open: that is what routes every typed
  -- character here instead of to the pane underneath. A float would grab input
  -- instead, but a float would also cover the matches — see the header.
  focusable = true,

  keys = {
    -- v1's chord. The kernel folds the three encodings terminals deliver it as
    -- (`ctrl+/`, `ctrl+7`, `ctrl+_`) into this one, so declaring it once is
    -- enough. It is not a bare `ctrl+<letter>`, so it reaches search even from a
    -- focused agent.
    {
      key = "ctrl+/",
      action = OPEN,
      desc = "search sessions",
      scope = "global",
      group = "UI",
    },
    -- Arrows, deliberately NOT `j`/`k`: those are letters, and a search box that
    -- cannot type `j` is not a search box. v1 draws the same line.
    --
    -- v1 also accepts `ctrl+p`/`ctrl+n` here, and this does not. In v1 the
    -- search focus captures input BEFORE the keybinding table, so it can shadow
    -- the global `ctrl+n` (new session); v2 routes every chord through one
    -- registry, where a plugin-scoped claim does not outrank a global one — so
    -- declaring it would take `ctrl+n` away from new-session everywhere, not
    -- only here. Rebind these two if you want that pair; they are declared, so
    -- the help editor can move them.
    { key = "down", action = NEXT, desc = "next result", group = "Search" },
    { key = "up", action = PREVIOUS, desc = "previous result", group = "Search" },
    { key = "enter", action = ACTIVATE, desc = "go to the result", group = "Search" },
    { key = "esc", action = CANCEL, desc = "close and put back", group = "Search" },
  },

  render = function(ctx)
    local width, height = ctx.width or 0, ctx.height or 0
    local search = load()
    want_content(search, ctx.elapsed or 0)
    save(search)
    local rows = results()
    search.cursor = widgets.clamp(search.cursor, #rows)

    -- Point the list at whatever is under the cursor now. Cheap because it only
    -- writes when the answer CHANGED: a frame that previews the row it previewed
    -- last time would fight the list for its own cursor.
    local current = rows[search.cursor]
    if current and current.id ~= search.previewed then
      preview(current)
      search.previewed = current.id
      save(search)
    end

    local scopes = summary(rows)
    local children = {
      query_row(search, rows),
    }
    if scopes then
      children[#children + 1] = {
        type = "text",
        len = 1,
        text = { { { text = " " .. scopes, style = { fg = theme.muted } } } },
      }
    end

    -- Rows the list will actually be given: the strip, less the panel's two
    -- borders, less every sibling already above it. Counted rather than written
    -- out, because a sibling added later would otherwise quietly take a row back
    -- from the list -- and asking for one row more than the rect holds does not
    -- clip. An over-subscribed box hands the whole rect to its FIRST child and
    -- nothing to the others, so one line paints and every result disappears.
    local consumed = 2
    for _, child in ipairs(children) do
      consumed = consumed + (child.len or 0)
    end

    local list_height = math.max(0, height - consumed)
    local lines, selected_line =
      result_rows(rows, search.cursor, math.max(0, width - 4), list_height)
    local list = widgets.list({
      rows = lines,
      selected = selected_line,
      height = list_height,
      empty = "  nothing matches",
    })
    list.fill = 1
    children[#children + 1] = list

    return {
      type = "box",
      frame = widgets.panel("Search", ctx.focused),
      children = children,
    }
  end,

  on_click = function(hit)
    if not hit.id then
      return false
    end
    local search = load()
    local rows = results()
    local index = widgets.index_of(rows, hit.id)
    if not index then
      return false
    end
    search.cursor = index
    save(search)
    preview(rows[index])
    return true
  end,

  on_action = function(action)
    if action == OPEN then
      -- A second press closes it, as every other panel key does. Cancelling
      -- rather than keeping, since nothing was chosen.
      if panels.shown(NAME) then
        close(load(), false)
        command("focus", { text = "sessions" })
        return true
      end
      local search = load()
      textinput.clear(search.field)
      search.cursor = 1
      -- Captured BEFORE anything is previewed, which is the only moment the
      -- pre-search selection is still readable.
      search.snapshot = {
        selected = store.selected,
        sessions_shown = panels.shown("sessions"),
      }
      save(search)
      store[QUERY] = ""
      panels.show(NAME)
      -- Preview at once, so the first `down` steps to the SECOND result rather
      -- than to the second while the list still shows the first. The snapshot
      -- above is what makes that safe to do before anything was chosen.
      preview(results()[1])
      command("focus", { text = NAME })
      return true
    end

    if not panels.shown(NAME) then
      return false
    end
    local search = load()
    local rows = results()

    if action == NEXT or action == PREVIOUS then
      local step = action == NEXT and 1 or -1
      search.cursor = widgets.clamp(search.cursor + step, #rows)
      local row = rows[search.cursor]
      search.previewed = row and row.id or nil
      save(search)
      preview(row)
      return true
    elseif action == ACTIVATE then
      local row = rows[widgets.clamp(search.cursor, #rows)]
      if not row then
        return true
      end
      preview(row)
      close(search, true)
      -- v1's Enter lands you IN the result: a session result focuses that
      -- session's terminal, not the row you picked it from.
      command("focus", { text = "agent" })
      return true
    elseif action == CANCEL then
      close(search, false)
      command("focus", { text = "sessions" })
      return true
    end
    return false
  end,

  --- Everything the declared keys did not take is typing.
  ---
  --- Returning true for any consumed key is what keeps a letter from reaching
  --- the pane underneath while the strip has focus.
  on_key = function(key)
    if not panels.shown(NAME) then
      return false
    end
    local search = load()
    if not textinput.key(search.field, key) then
      return false
    end
    save(search)
    store[QUERY] = search.field.value or ""
    -- The preview follows the query as well as the arrows — otherwise typing
    -- narrows the list while the list underneath still shows the old row. It is
    -- `render` that does it, on the frame this keystroke causes: it clamps the
    -- cursor to the new results and previews whatever that lands on, which is
    -- the same answer this handler used to compute. Computing it here as well
    -- scanned every session (and, once a query settles, every session's SCREEN)
    -- a second time per keystroke, for a row that was about to be previewed
    -- anyway.
    return true
  end,
}
