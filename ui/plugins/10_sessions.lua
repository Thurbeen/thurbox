-- The session list.
--
-- In v1 this was `ui::project_list` — 2,212 lines of Rust, welded to a 616-method
-- `App` struct. Here it is a file you can edit while thurbox is running.
--
-- It is an ORDINARY plugin. The kernel has no session-list concept: it hands
-- over a snapshot and this decides everything about how the list looks.
--
-- This file reproduces v1's rendering cell for cell: the `── repo ──` rule
-- headers, the `└`/`↳`/`⇅`/`⑂` marks, the full-width selection bar, the status
-- dot strip on the top border, and the `▲ N`/`▼ N` scroll indicators overlaid on
-- the border rows.
--
-- WHY THE BORDER IS DRAWN BY HAND. `Frame.title` is a plain, unstyled,
-- left-aligned string, so a frame cannot express any of the three things v1 puts
-- on its border: the focused title *badge* (inverted fg on an accent
-- background), the right-aligned per-session status dots, or the scroll counts
-- overlaid on the border cells. Drawing a header line inside the frame would
-- cost a content row and so would not be parity. Composing the border out of
-- `text` nodes costs nothing — the border rows are the rows the frame would have
-- occupied — and stays inside the four-kind vocabulary.

local fuzzy = require("lib.fuzzy")
local hover = require("lib.hover")
local panels = require("lib.panels")
local plugin_settings = require("lib.settings")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

-- ── Text helpers the contract needs and widgets.lua does not have ───────────

--- Character count across a span list (widgets.len handles one string).
local function spans_len(spans)
  local total = 0
  for _, span in ipairs(spans) do
    total = total + widgets.len(span.text or "")
  end
  return total
end

--- Append a raw-space span so a styled run covers the full width — how v1 makes
--- the selection background reach the right edge.
local function pad_spans(spans, width)
  local short = width - spans_len(spans)
  if short > 0 then
    spans[#spans + 1] = { text = string.rep(" ", short) }
  end
  return spans
end

--- ratatui's `Style::patch` over every span: the overlay wins for the fields it
--- sets, the span keeps the rest.
---
--- `keep_fg` names spans whose COLOUR is a signal of its own — the characters a
--- search query matched. They still take everything else the overlay sets, so the
--- selection bar paints through them (a bar with a gap in it is not a bar), but
--- their foreground survives it. v1 layered the same two the same way round:
--- `highlight_style` was built ON TOP of the row's base style
--- (`src/ui/highlight.rs`), so an accent match stayed accent on the selected row.
--- Patching over it left the match wearing the selection's own colour with only
--- its underline showing — on the one row the strip was pointing at, since
--- previewing a result moves this list's cursor onto it.
local function patch_spans(spans, style, keep_fg)
  for index, span in ipairs(spans) do
    local merged = {}
    for key, value in pairs(span.style or {}) do
      merged[key] = value
    end
    for key, value in pairs(style) do
      if not (keep_fg and keep_fg[index] and key == "fg") then
        merged[key] = value
      end
    end
    span.style = merged
  end
  return spans
end

--- First `count` characters, cut on a character boundary.
-- ── Border composition ─────────────────────────────────────────────────────
--
-- A border row is built as a cell buffer so segments can be *layered* the way
-- v1 layers them: the dot strip is a right-aligned title, and the scroll count
-- is painted over the same cells afterwards.

local function new_cells(width, char, style)
  local cells = {}
  for index = 1, width do
    cells[index] = { ch = char, style = style }
  end
  return cells
end

--- Paint `text` into the buffer starting at `at` (1-based). Out-of-range cells
--- are dropped, which is how an over-long title clips at either edge.
local function place_text(cells, at, text, style)
  local index = at
  for _, code in utf8.codes(text or "") do
    if index >= 1 and index <= #cells then
      cells[index] = { ch = utf8.char(code), style = style }
    end
    index = index + 1
  end
  return index
end

local function place_spans(cells, at, spans)
  local index = at
  for _, span in ipairs(spans) do
    index = place_text(cells, index, span.text, span.style)
  end
  return index
end

--- Coalesce the buffer back into spans. Styles are compared by identity, which
--- is exact here because every segment is painted with one shared style table.
local function cells_to_spans(cells)
  local spans, current, current_style = {}, nil, nil
  for _, cell in ipairs(cells) do
    if current and cell.style == current_style then
      current.text = current.text .. cell.ch
    else
      current = { text = cell.ch, style = cell.style }
      current_style = cell.style
      spans[#spans + 1] = current
    end
  end
  return spans
end

-- ── Focus styling ──────────────────────────────────────────────────────────
--
-- v1's three levels (`ui::FocusLevel`), and which this pane uses.
--
-- The kernel publishes a single `focused` boolean, and it is the PANE that knows
-- what its unfocused state means — the same split v1 makes, deciding per pane in
-- `view.rs` rather than in the widget. For the session list the answer is
-- `Active`: it always shows the current session, so it stays contextually
-- relevant with focus elsewhere and keeps the plain accent border. v1's
-- `list_focus` falls through to exactly that.
--
-- `Inactive` (the gray border) is therefore unreached here today. It is kept
-- because it is not dead: v1 uses it for this pane while the automations context
-- owns the centre, so the pane that returns brings the level back with it.

local function border_style(level)
  if level == "focused" then
    return { fg = theme.accent_bright }
  elseif level == "active" then
    return { fg = theme.accent }
  end
  return { fg = theme.border }
end

local function title_style(level)
  if level == "focused" then
    -- v1's `Theme::focused_title()`: a badge, not coloured text.
    return { fg = theme.role("inverted_fg"), bg = theme.accent, bold = true }
  elseif level == "active" then
    return { fg = theme.accent }
  end
  return { fg = theme.border }
end

-- ── The model: what to draw, before any width is known ──────────────────────

local NO_REPO = "(no repo)"

--- In-flight commands, keyed by the session they concern.
---
--- A command is accepted instantly and lands in a later snapshot, so without
--- this a restart would look like nothing happened for a moment. v1 needed a
--- whole `PendingSpawn` type for the same reason. A delete is the exception:
--- see `live_sessions()`, which drops the row instead of annotating it.
local function pending()
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

--- The rows to draw, in order, before any of them is turned into text.
---
--- An item is one selectable unit — a session row plus, for a group's first
--- row, the group header glued on top. That gluing is v1's: the header is line
--- zero of the first session's list item, so clicking a header selects that
--- session and the scroll window can never separate the two.
--- Whether the list draws repo headers.
---
--- v1 always groups; a user with one repo sees a header that says nothing, so
--- this is a knob rather than a rule. The GROUPING still happens either way --
--- only the header line is suppressed -- so ordering and the move-past-a-group
--- behaviour are unchanged.
local function grouped()
  return plugin_settings.enabled("sessions", "group_by_repo", true)
end

local function build_model(rows)
  local items = {}
  -- Dropped before anything is grouped or ordered, so every consumer of the
  -- model agrees: the cursor lands on the next row, the border dots lose one,
  -- and a group whose last session went takes its header with it.
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
    -- manual order among siblings and among roots.
    local nested, seen = {}, {}
    local function emit(index, depth)
      local session = rows[index]
      if seen[session.id] then
        return
      end
      seen[session.id] = true
      nested[#nested + 1] = { index = index, depth = depth }
      for _, other in ipairs(group.members) do
        if rows[other].parent == session.id then
          emit(other, depth + 1)
        end
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
        header = (first and grouped()) and group.label or nil,
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
        header = (first and grouped()) and group.label or nil,
        target = false,
      }
      first = false
    end
  end

  return items
end

-- ── Turning a model item into lines ────────────────────────────────────────

--- `── label ────────`, muted, full bleed. The header never reflects selection:
--- highlighting belongs to the session rows alone.
local function header_line(label, inner_width)
  local text = "── " .. label .. " "
  local used = widgets.len(text)
  if inner_width > used then
    text = text .. string.rep("─", inner_width - used)
  end
  return { { text = text, style = { fg = theme.muted } } }
end

local function status_glyph(status, elapsed)
  local spec = theme.status(status)
  if status == "working" then
    local frame = math.floor((elapsed or 0) * 8) % #theme.spinner + 1
    return theme.spinner[frame], spec.color
  end
  return spec.glyph, spec.color
end

--- The status text that follows the name. v1 shows the agent's notification (or
--- the word "Blocked") for a blocked row, and the OSC activity title otherwise;
--- a row with neither carries no text, because the coloured dot already says
--- what state it is in.
---
--- Both come off the agent's own terminal — the activity line is its OSC window
--- title, the notification its OSC 9/777 message — so they are published from
--- the live pane rather than the database.
local function agent_status_text(session)
  -- Nothing the agent said can be current on a host we cannot reach, so the
  -- row says why instead of showing a last message as if it were live.
  if session.status == "unreachable" then
    if session.remote_host then
      return "host " .. session.remote_host .. " unreachable"
    end
    return "unreachable"
  end
  if session.status == "blocked" then
    return session.notification or "Blocked"
  end
  local activity = session.activity
  if activity then
    activity = activity:match("^%s*(.-)%s*$")
    if activity ~= "" then
      return activity
    end
  end
  return nil
end

--- Append the trailing status, budgeted against the width actually available.
--- Dropped rather than overflowed, exactly as v1 drops it.
local SEPARATOR = "  "
local MIN_WIDTH = 4

local function push_status(spans, text, style, inner_width)
  if not text or text == "" then
    return
  end
  local used = spans_len(spans) + widgets.len(SEPARATOR)
  local avail = math.max(0, inner_width - used)
  if avail >= MIN_WIDTH then
    spans[#spans + 1] = { text = SEPARATOR }
    spans[#spans + 1] = { text = widgets.truncate_hard(text, avail), style = style }
  end
end

--- The live search query, or nil when nothing is being searched.
---
--- Read from `store` rather than handed over by the search pane: the pane
--- holding a row is what knows how a highlight should look in it, so search
--- publishes WHAT it is looking for and each pane answers for its own rows. The
--- kernel's `decorate` hook does the same job for panes search cannot expect to
--- cooperate; this list is one of its own.
local function search_query()
  local text = store["search.query"]
  if type(text) ~= "string" or text == "" then
    return nil
  end
  return text
end

--- Where the query hits a session's name, or nil when the row does not match at
--- all. `false` means it matched on something else — the row stays lit but its
--- name carries no marks.
local function name_hits(session, text)
  if not text then
    return nil
  end
  local positions = fuzzy.match(text, session.name or "")
  if positions then
    return positions
  end
  -- `or ""` on every element, because `ipairs` STOPS AT THE FIRST NIL. A session
  -- with no worktree has `session.branch == nil` in the middle of this list, so
  -- the scan halted after `agent` and its repository was never tested — the row
  -- was then dimmed as a non-match while the search strip counted it as a match
  -- and annotated it `repo: …`. The two halves of one feature disagreed on the
  -- same frame, and only for sessions without a branch, which is why the one
  -- worktree session in a review capture looked like the only correct row.
  for _, field in ipairs({ session.agent or "", session.branch or "", session.repo or "" }) do
    if field ~= "" and fuzzy.match(text, field) then
      return false
    end
  end
  return nil
end

local function session_line(item, inner_width, elapsed, is_selected, work)
  local session = item.session
  local glyph, glyph_color = status_glyph(session.status, elapsed)
  local status_style = { fg = glyph_color }
  -- A blocked row's text is an attention message, so it keeps the dot's colour;
  -- plain activity is muted, leaving the name the row's visual anchor. v1 draws
  -- the same split.
  local trailing = agent_status_text(session)
  local trailing_style = status_style
  if session.status ~= "blocked" then
    trailing_style = { fg = theme.muted }
  end

  -- Work already accepted but not yet in the snapshot is the more recent truth,
  -- so it takes the dot's place. v1 has no equivalent for a live row; the
  -- geometry is v1's, the signal is v2's.
  if work then
    if work.phase == "failed" then
      glyph, status_style = "✗", { fg = theme.role("status_error") }
      trailing = work.error and ("failed: " .. work.error) or "failed"
      trailing_style = { fg = theme.role("status_error") }
    else
      glyph, status_style = "◌", { fg = theme.muted }
      trailing, trailing_style = work.kind, { fg = theme.muted }
    end
  end

  local spans = { { text = " " .. glyph .. " ", style = status_style } }

  -- Nesting prefix: a tree mark for a child inside the group, a lone mark for
  -- one whose parent renders elsewhere in the list.
  if item.depth > 0 then
    spans[#spans + 1] = {
      text = string.rep("  ", item.depth - 1) .. "└ ",
      style = { fg = theme.muted },
    }
  elseif item.cross_group then
    spans[#spans + 1] = { text = "↳ ", style = { fg = theme.muted } }
  end

  -- An agent running on another machine, then a session that owns a worktree.
  if session.host then
    spans[#spans + 1] = { text = "⇅ ", style = { fg = theme.accent } }
  end
  if (session.worktrees or 0) > 0 then
    spans[#spans + 1] = { text = "⑂ ", style = { fg = theme.branch } }
  end

  -- Never truncated: the name is the row's anchor, and overflow clips.
  local query = search_query()
  local hits = name_hits(session, query)
  local name_style = is_selected and { fg = theme.role("selection_fg"), bold = true }
    or { fg = theme.text }
  -- Which spans the search highlight owns, by position in `spans`. The overlays
  -- below are told, so the selection bar cannot repaint a match — see
  -- `patch_spans`. Identity on the style table is what marks one: `fuzzy.spans`
  -- hands back the very table it was given for a matched run.
  local matched_spans = nil
  if hits then
    local hit_style = { fg = theme.accent, bold = true, underline = true }
    matched_spans = {}
    for _, span in ipairs(fuzzy.spans(session.name or "?", hits, name_style, hit_style)) do
      spans[#spans + 1] = span
      if span.style == hit_style then
        matched_spans[#spans] = true
      end
    end
  else
    spans[#spans + 1] = { text = session.name or "?", style = name_style }
  end

  push_status(spans, trailing, trailing_style, inner_width)

  -- A row nothing matched is dimmed rather than hidden: v1 keeps every row on
  -- screen and lets the contrast do the filtering, so the list never jumps
  -- around under a cursor you are still moving.
  if query and hits == nil then
    patch_spans(spans, { fg = theme.muted, bold = false, underline = false })
  end

  -- The selection bar is painted here rather than by a list-wide highlight,
  -- which would bleed onto the group header glued above a group's first row.
  if is_selected then
    pad_spans(spans, inner_width)
    patch_spans(spans, {
      bg = theme.role("selection_bg"),
      fg = theme.role("selection_fg"),
      bold = true,
    }, matched_spans)
  elseif hover.id(session.id) then
    -- v1's row hover: a subtle band marking what a click would hit. Only the
    -- BACKGROUND is tinted — each cell keeps its own fg, so the status dot and
    -- the branch colour survive being hovered. A button gets the stronger
    -- accent fill instead (see the agent pane's chips); a row is not a button.
    --
    -- Skipped on the selected row because v1 tints it to the colour it already
    -- has, which is no change at all.
    pad_spans(spans, inner_width)
    patch_spans(spans, { bg = theme.role("selection_bg") })
  end

  return spans
end

--- v1's phase vocabulary, which the placeholder row shows beside the label.
local PHASE_LABEL = {
  queued = "creating…",
  running = "creating…",
  resolving = "setting up…",
  worktrees = "creating…",
  backend = "setting up…",
  launching = "spawning…",
  persisting = "spawning…",
}

local function pending_line(command, inner_width, elapsed)
  local failed = command.phase == "failed"
  local glyph, glyph_style
  if failed then
    glyph, glyph_style = "✗", { fg = theme.role("status_error") }
  else
    -- A spinner only while something is actually running.
    local frame = math.floor((elapsed or 0) * 8) % #theme.spinner + 1
    glyph, glyph_style = theme.spinner[frame], { fg = theme.warn }
  end

  local label = command.subject or "new session"
  local phase
  if failed then
    phase = command.error and ("failed: " .. command.error) or "failed"
  else
    phase = PHASE_LABEL[command.phase] or "creating…"
  end

  local spans = {
    { text = " " .. glyph .. " ", style = glyph_style },
    { text = label, style = { fg = theme.secondary } },
  }
  -- Drop the phase rather than overflow a narrow panel.
  local used = 3 + widgets.len(label)
  if inner_width > used + widgets.len(phase) + 2 then
    spans[#spans + 1] = { text = "  " .. phase, style = { fg = theme.muted } }
  end
  return spans
end

-- ── Scrolling: ratatui's ListState semantics over variable item heights ─────

--- The minimal scroll that keeps `selected` fully visible, given each item's
--- height. Returns the first item (1-based) and the count of items after it
--- that fit — v1's `visible_count_from_heights`.
local function scroll_window(heights, offset, selected, max_height)
  local count = #heights
  if count == 0 or max_height <= 0 then
    return 1, 0
  end

  -- 0-based inside, to stay recognisably ratatui's arithmetic.
  local first = math.max(0, math.min(offset, count - 1))
  local last = first
  local used = 0
  for index = first, count - 1 do
    if used + heights[index + 1] > max_height then
      break
    end
    used = used + heights[index + 1]
    last = last + 1
  end

  local target = math.max(0, math.min(selected - 1, count - 1))
  while target >= last do
    used = used + heights[last + 1]
    last = last + 1
    while used > max_height do
      used = used - heights[first + 1]
      first = first + 1
    end
  end
  while target < first do
    first = first - 1
    used = used + heights[first + 1]
    while used > max_height do
      last = last - 1
      used = used - heights[last + 1]
    end
  end

  -- v1 recounts what fits from the settled offset, so the "below" count never
  -- claims a partially drawn item is visible.
  local visible, consumed = 0, 0
  for index = first, count - 1 do
    if consumed + heights[index + 1] > max_height then
      break
    end
    consumed = consumed + heights[index + 1]
    visible = visible + 1
  end
  return first + 1, visible
end

--- Move the cursor by `step`, skipping items that select nothing.
local function move(items, from, step)
  local count = #items
  if count == 0 then
    return 1
  end
  local at = from
  for _ = 1, count do
    at = (at - 1 + step) % count + 1
    if items[at].target then
      return at
    end
  end
  return from
end

local function sessions()
  return thurbox and thurbox.sessions or {}
end

--- Is deleting reversible?
---
--- `[features] soft_delete` off means the TUI deletes for real — v1's own
--- behaviour, and why the confirmation below exists: there is no Ctrl+Z for it.
local function soft_delete()
  local settings = thurbox and thurbox.settings
  if not settings or not settings.features then
    return true
  end
  return settings.features.soft_delete ~= false
end

--- What deleting this session would destroy, itemised — or nil when it would
--- destroy nothing.
---
--- v1's `DeleteRisk::from_stats`, its decision included: work is at risk when
--- there are uncommitted or untracked files, commits that exist nowhere else,
--- or — the case that matters most — a state that could not be read at all,
--- which is reported rather than assumed clean. Everything else is a
--- known-clean session, and nil says so.
---
--- The worktree *directory* is deliberately not a reason to ask: force-delete
--- removes the checkout and leaves the branch, so a clean one comes back from
--- it. It is listed as context for a question already owed, never as the cause.
local function at_risk(session)
  local git = session.git
  if not git then
    -- Not computed yet, not a git worktree, or a host that could not be
    -- reached. v1 confirms rather than assume clean.
    return { "its state could not be read" }
  end

  local lines = {}
  local uncommitted = (git.files or 0) + (git.untracked or 0)
  if uncommitted > 0 then
    lines[#lines + 1] = uncommitted .. " uncommitted or untracked file(s)"
  elseif git.dirty then
    -- `dirty` is any `status --porcelain` output, so it outlives a count of
    -- zero (a mode change, a submodule). Still work, still unrecoverable.
    lines[#lines + 1] = "uncommitted changes"
  end
  if (git.ahead or 0) > 0 then
    lines[#lines + 1] = git.ahead .. " commit(s) not pushed anywhere else"
  end

  -- Everything above speaks for the session's *primary* directory, which is the
  -- only one the snapshot stats. v1 inspected every worktree it was about to
  -- remove, so on a session that owns several the rest are unknown rather than
  -- clean — a reason to ask in its own right.
  local worktrees = session.worktrees or 0
  if worktrees > 1 then
    lines[#lines + 1] = "its other worktrees could not be read"
  end

  if #lines == 0 then
    return nil
  end

  -- What else goes, once a question is owed. Never the reason for one.
  if worktrees == 1 then
    lines[#lines + 1] = "its worktree directory"
  elseif worktrees > 1 then
    lines[#lines + 1] = "its " .. worktrees .. " worktree directories"
  end
  return lines
end

--- Delete for good, asking first only when there is something to lose.
---
--- v1's `App::delete_active_session`: it assessed the risk and opened
--- `ConfirmDelete` only for `Some(risk)`, deleting a known-clean session on the
--- keystroke. Asking about nothing trains the answer, which is the opposite of
--- what a confirmation is for.
---
--- The question travels through `store`, so the confirm plugin needs to know
--- nothing about sessions.
local function delete_for_good(session, question)
  local lines = at_risk(session)
  if not lines then
    command("delete", { session = session.id, force = true })
    return
  end
  store.confirm = {
    question = question,
    lines = lines,
    command = "delete",
    options = { session = session.id, force = true },
  }
end

--- The chord bound to opening the creation flow, if anything is.
---
--- Read from the registry rather than hardcoded, because it is rebindable and
--- because the flow is removable: the empty state must not name a key that
--- resolves to nothing.
local function new_session_chord()
  for _, binding in ipairs((thurbox and thurbox.registry and thurbox.registry.keys) or {}) do
    if binding.action == "new_session.open" then
      return binding.key
    end
  end
  return nil
end

-- ── Manual ordering ─────────────────────────────────────────────────────────
--
-- A move is computed HERE, over the rendered items, and sent as an explicit
-- order. The kernel cannot compute it: only this pane knows the repo grouping
-- and the parent/child nesting, and therefore what a move actually swaps — a
-- root row drags its whole subtree, a root row at its group's edge moves the
-- WHOLE GROUP past the neighbouring one, and a nested child moves among its
-- siblings only. Ported from v1's `move_in_order`.

--- End of the block rooted at `at`: the first later row at a depth `<=` its own,
--- i.e. one past its whole rendered subtree.
local function block_end(items, at)
  local last = #items
  local finish = at + 1
  while finish <= last and items[finish].depth > items[at].depth do
    finish = finish + 1
  end
  return finish
end

--- Start of the group containing `at`: the nearest row at or above it that
--- carries a header.
---
--- Only the first row of a group carries one, and with `group_by_repo` off
--- nothing does -- so the answer there is row 1: the whole list is one group. It
--- used to be nil, which made `root_ranges` give up and every root move a silent
--- no-op for anyone who had turned grouping off.
local function group_start(items, at)
  for index = at, 1, -1 do
    if items[index].header then
      return index
    end
  end
  return #items > 0 and 1 or nil
end

--- Start of the group after the one starting at `at`, or one past the end.
local function group_end(items, at)
  for index = at + 1, #items do
    if items[index].header then
      return index
    end
  end
  return #items + 1
end

--- The two adjacent ranges a root move swaps: the neighbouring root block in
--- the same group, or — at a group edge — this whole group with its neighbour.
local function root_ranges(items, at, down)
  local last = #items
  local finish = block_end(items, at)
  local gs = group_start(items, at)
  if not gs then
    return nil
  end
  local ge = group_end(items, gs)

  if down then
    if finish < ge then
      return at, finish, finish, block_end(items, finish)
    elseif ge <= last then
      return gs, ge, ge, group_end(items, ge)
    end
    return nil
  end
  if at > gs then
    for index = at - 1, gs, -1 do
      if items[index].depth == 0 then
        return index, at, at, finish
      end
    end
    return nil
  end
  if gs > 1 then
    local previous = group_start(items, gs - 1)
    if previous then
      return previous, gs, gs, ge
    end
  end
  return nil
end

--- The two adjacent ranges a nested move swaps: the adjacent same-depth sibling
--- only, so a child never leaves its parent.
local function child_ranges(items, at, down)
  local last = #items
  local depth = items[at].depth
  local finish = block_end(items, at)

  if down then
    -- A shallower row where the next sibling would start means the parent's
    -- subtree ended.
    if finish <= last and items[finish].depth == depth then
      return at, finish, finish, block_end(items, finish)
    end
    return nil
  end
  -- Scan back over the previous sibling's subtree; a same-depth row is that
  -- sibling, a shallower one is our parent.
  local index = at - 1
  while index >= 1 and items[index].depth > depth do
    index = index - 1
  end
  if index >= 1 and items[index].depth == depth then
    return index, at, at, finish
  end
  return nil
end

--- The rendered item list with the block at `at` moved one place, or nil when
--- it is already at the edge that move would take it past.
local function move_block(items, at, down)
  local a_start, a_end, b_start, b_end
  if items[at].depth == 0 then
    a_start, a_end, b_start, b_end = root_ranges(items, at, down)
  else
    a_start, a_end, b_start, b_end = child_ranges(items, at, down)
  end
  if not a_start then
    return nil
  end
  local moved = {}
  for index = 1, a_start - 1 do
    moved[#moved + 1] = items[index]
  end
  for index = b_start, b_end - 1 do
    moved[#moved + 1] = items[index]
  end
  for index = a_start, a_end - 1 do
    moved[#moved + 1] = items[index]
  end
  for index = b_end, #items do
    moved[#moved + 1] = items[index]
  end
  return moved
end

--- Sort by name **within each repo group**, preserving group order and the
--- parent/child nesting: roots sort among themselves, each parent's children
--- among theirs. v1's `sort_alphabetically_within_groups`.
local function sorted_within_groups(items)
  local out = {}
  local at = 1
  while at <= #items do
    local group_last = group_end(items, at) - 1
    -- Collect this group's root blocks, each as its own subtree.
    local blocks = {}
    local index = at
    while index <= group_last do
      local finish = block_end(items, index)
      local block = {}
      for inner = index, finish - 1 do
        block[#block + 1] = items[inner]
      end
      blocks[#blocks + 1] = block
      index = finish
    end
    -- Case-insensitive, like v1, and stable on a tie so equal names keep their
    -- existing relative order.
    for position, block in ipairs(blocks) do
      block.position = position
    end
    table.sort(blocks, function(a, b)
      local left = (a[1].session.name or ""):lower()
      local right = (b[1].session.name or ""):lower()
      if left == right then
        return a.position < b.position
      end
      return left < right
    end)
    for _, block in ipairs(blocks) do
      for _, item in ipairs(block) do
        out[#out + 1] = item
      end
    end
    at = group_last + 1
  end
  return out
end

--- Persist a rendered order. Header ownership is a *rendering* property of the
--- first row in a group, so it is left to the next build rather than carried.
local function persist_order(items)
  local ids = {}
  for _, item in ipairs(items) do
    if item.target then
      ids[#ids + 1] = item.target
    end
  end
  if #ids > 0 then
    command("order", { list = ids })
  end
end

return {
  name = "sessions",
  slot = "sessions",
  order = 10,
  focusable = true,
  -- This render reads `thurbox.*` and `ctx` and writes nothing, so the kernel
  -- may reuse the tree it returned while neither has changed. The working
  -- spinner still animates: the kernel drops the cached tree when the shared
  -- animation clock moves, and that clock ticks at the same rate
  -- `widgets.status_glyph` advances the spinner at — but only while something
  -- is actually animating, so an idle list settles instead of re-rendering.
  pure = true,

  -- Declared as DATA, not just handled. That is what lets the kernel list these
  -- in help, detect a clash with another plugin, and let you rebind them —
  -- none of which it could do if they only existed inside on_key.
  --- Declared as data, so the settings modal renders a row for it without
  --- knowing what a repo group is. Read back through `lib.settings`.
  settings = {
    {
      id = "group_by_repo",
      desc = "Group sessions under a repo header",
      default = true,
    },
  },

  keys = {
    { key = "j", action = "sessions.next", desc = "next session", group = "Navigation" },
    { key = "k", action = "sessions.previous", desc = "previous session", group = "Navigation" },
    -- The arrows alongside j/k, which is what v1 binds by default
    -- (`Action::SessionListNext` = `j` and `Down`). Two chords for one action, so
    -- both appear in help and either can be rebound on its own.
    { key = "down", action = "sessions.next", desc = "next session", group = "Navigation" },
    { key = "up", action = "sessions.previous", desc = "previous session", group = "Navigation" },
    { key = "g", action = "sessions.first", desc = "first session", group = "Navigation" },
    -- v1's `Enter` on a session row: go to what you selected. The row is already
    -- selected by the time this fires, so opening is only a focus change.
    { key = "enter", action = "sessions.open", desc = "open the session", group = "Navigation" },
    { key = "d", action = "sessions.delete", desc = "delete session", group = "Sessions" },
    {
      key = "D",
      action = "sessions.force_delete",
      desc = "delete session and its worktree",
      group = "Sessions",
    },
    { key = "r", action = "sessions.restart", desc = "restart session", group = "Sessions" },
    { key = "J", action = "sessions.move_down", desc = "move session down", group = "Sessions" },
    { key = "K", action = "sessions.move_up", desc = "move session up", group = "Sessions" },
    { key = "S", action = "sessions.sort", desc = "sort sessions by name", group = "Sessions" },

    -- v1's global session chords, which fire from any pane. Every one of them
    -- is in v1's `Action::terminal_passthrough` set — they are readline's
    -- (Ctrl+D EOF, Ctrl+R reverse-search, Ctrl+S XOFF, Ctrl+F forward-char,
    -- Ctrl+O operate-and-get-next) — so a focused agent terminal keeps the
    -- keystroke and the command stays reachable from every other pane.
    {
      key = "ctrl+d",
      action = "sessions.delete",
      desc = "delete session",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+r",
      action = "sessions.restart",
      desc = "restart session",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+f",
      action = "sessions.fork",
      desc = "fork session",
      scope = "global",
      passthrough = true,
      group = "Sessions",
    },
    {
      key = "ctrl+s",
      action = "sessions.sync",
      desc = "sync worktree with its base branch",
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
    -- Not passthrough, matching v1: undo and session navigation are how you
    -- act on the list without leaving the terminal you are watching.
    {
      key = "ctrl+z",
      action = "sessions.undo",
      desc = "undo the last delete",
      scope = "global",
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
    -- F9 alone, as in v1: the session column is the one panel toggle with no
    -- readline chord to collide with, so it needs no Ctrl primary and no
    -- passthrough exception.
    {
      key = "f9",
      action = "sessions.toggle_panel",
      desc = "toggle the session list",
      scope = "global",
      group = "Panels",
    },
  },

  render = function(ctx)
    -- ctx.width/height are THIS PANE's, not the screen's.
    local width = math.max(0, ctx.width or 0)
    local height = math.max(0, ctx.height or 0)
    local level = ctx.focused and "focused" or "active"
    local frame_style = border_style(level)
    if width < 2 or height < 2 then
      return { type = "text", text = "" }
    end
    local inner_width = width - 2
    local inner_height = height - 2

    local list = sessions()
    local items = build_model(list)
    local busy = pending()

    -- Keep the cursor on the session it was on, not on a row number.
    --
    -- A reorder is a command: it lands a frame or two later, and the row that
    -- was under the cursor has moved by then. Following the id instead means
    -- holding J walks a session down the list, which is what you meant.
    local cursor = state.cursor or 1
    -- An outside request to select a session: a clicked OS notification, or
    -- `thurbox-cli session focus`. Consumed here and cleared, because the cursor
    -- is republished from this pane every frame — anything that merely wrote
    -- `store.selected` would be overwritten a frame later.
    if store.focus_session then
      state.follow = store.focus_session
      store.focus_session = nil
    end
    -- Another PANE may steer the selection by writing `store.selected` — the
    -- search strip jumping to a result, a task opening the session it spawned.
    -- Publishing the cursor every frame would undo that write a frame later, so a
    -- value this pane did not publish is read as a request and followed. v1's
    -- panes call `App::select_session` for the same reason.
    local steered = store.selected
    if steered and steered ~= state.published then
      state.follow = steered
    end
    if state.follow then
      for index, item in ipairs(items) do
        if item.target == state.follow then
          cursor = index
          break
        end
      end
    end
    cursor = widgets.clamp(cursor, #items)
    if #items > 0 and not items[cursor].target then
      cursor = move(items, cursor, 1)
    end
    state.cursor = cursor
    -- Publish the selection so the agent pane knows what to show, remembering
    -- what was published so an outside write can be told from our own echo.
    local target = items[cursor] and items[cursor].target or nil
    store.selected = target
    state.published = target

    -- One dot per session on the top border, in render order, each in its own
    -- status colour. Suppressed entirely when there are no sessions.
    local dots = {}
    for _, item in ipairs(items) do
      if item.kind == "session" then
        local glyph, color = status_glyph(item.session.status, ctx.elapsed)
        dots[#dots + 1] = { text = glyph, style = { fg = color } }
      end
    end

    local function row(spans, id, class)
      return {
        type = "box",
        axis = "horizontal",
        len = 1,
        children = {
          { type = "text", len = 1, text = { { { text = "│", style = frame_style } } } },
          {
            type = "text",
            fill = 1,
            text = { spans },
            id = id,
            class = class,
            -- Decoration (the search plugin) finds rows by this role, and only
            -- the content is inside it — so a highlight can never repaint the
            -- border cells.
            role = id and "row" or nil,
          },
          { type = "text", len = 1, text = { { { text = "│", style = frame_style } } } },
        },
      }
    end

    local lines, above, below = {}, 0, 0

    if #items == 0 then
      -- v1's placeholder: a blank line, then two centred muted lines.
      local function centred(text)
        local pad = math.max(0, math.floor((inner_width - widgets.len(text)) / 2))
        return { { text = string.rep(" ", pad) .. text, style = { fg = theme.muted } } }
      end
      -- `{ spans = … }`, like every other entry: the row builder below reads
      -- `line.spans`, so a bare span list here renders as a blank row — which is
      -- exactly what the placeholder did until a test looked at it.
      lines[1] = { spans = {} }
      lines[2] = { spans = centred("No sessions yet") }
      -- v1's second line names the chord that creates one, and it is only shown
      -- when something actually answers it: the chord is looked up in the
      -- registry rather than written here, so a rebind — or the flow being
      -- removed — cannot leave the empty state advertising a key that does
      -- nothing.
      local chord = new_session_chord()
      if chord then
        lines[3] = { spans = centred("Press " .. chord .. " to create one") }
      end
    else
      local heights = {}
      for index, item in ipairs(items) do
        heights[index] = item.header and 2 or 1
      end

      local first, visible = scroll_window(heights, state.offset or 0, cursor, inner_height)
      state.offset = first - 1
      above = first - 1
      below = #items - (first - 1 + visible)

      for index = first, #items do
        local item = items[index]
        if #lines >= inner_height then
          break
        end
        if item.header then
          lines[#lines + 1] = { spans = header_line(item.header, inner_width) }
        end
        if #lines < inner_height then
          if item.kind == "pending" then
            lines[#lines + 1] = {
              spans = pending_line(item.command, inner_width, ctx.elapsed),
              class = "pending-row",
            }
          else
            lines[#lines + 1] = {
              spans = session_line(
                item,
                inner_width,
                ctx.elapsed,
                index == cursor,
                busy[item.session.id]
              ),
              id = item.session.id,
              class = "session-row",
            }
          end
        end
      end
    end

    local children = {}

    -- Top border: the title badge at the left, the dot strip right-aligned, and
    -- the "items above" count painted over it — the same layering v1 gets from
    -- a right-aligned title plus a paragraph drawn on the border cells.
    local top = new_cells(width, "─", frame_style)
    top[1] = { ch = "╭", style = frame_style }
    top[width] = { ch = "╮", style = frame_style }
    place_spans(top, 2, { { text = " Sessions ", style = title_style(level) } })
    if #dots > 0 then
      place_spans(top, width - spans_len(dots), dots)
    end
    if above > 0 then
      local text = "▲ " .. above .. " "
      place_text(top, width - widgets.len(text), text, { fg = theme.muted })
    end
    children[#children + 1] = { type = "text", len = 1, text = { cells_to_spans(top) } }

    for index = 1, inner_height do
      local line = lines[index]
      children[#children + 1] =
        row(line and line.spans or {}, line and line.id, line and line.class)
    end

    local bottom = new_cells(width, "─", frame_style)
    bottom[1] = { ch = "╰", style = frame_style }
    bottom[width] = { ch = "╯", style = frame_style }
    if below > 0 then
      local text = "▼ " .. below .. " "
      place_text(bottom, width - widgets.len(text), text, { fg = theme.muted })
    end
    children[#children + 1] = { type = "text", len = 1, text = { cells_to_spans(bottom) } }

    return { type = "box", children = children }
  end,

  -- A click on a row selects it, exactly as `j`/`k` would — v1's
  -- `ClickAction::SelectSession`. The row carries the session id rather than an
  -- index, so a list that reordered between the paint and the press still
  -- selects the session you pointed at.
  --
  -- A repo header carries no id — it is drawn as its own line and only session
  -- lines are targets — so a click on one focuses the column and selects
  -- nothing. v1 folds the header into its group's first hitbox instead; giving
  -- it an id here would also hand it `role = "row"`, which the search
  -- decorator matches on, so the divergence is deliberate.
  on_click = function(hit)
    if not hit.id then
      return false
    end
    local items = build_model(sessions())
    for index, item in ipairs(items) do
      if item.target == hit.id then
        state.follow = nil
        state.cursor = index
        store.selected = hit.id
        state.published = hit.id
        return true
      end
    end
    return false
  end,

  on_action = function(action)
    -- The two that own no row, handled before the "is there a session" guard:
    -- hiding the column and undoing a delete both work on an empty list.
    if action == "sessions.toggle_panel" then
      panels.toggle("sessions")
      return true
    elseif action == "sessions.undo" then
      -- v1's Ctrl+Z undoes the delete YOU just did — `App::undo_delete`
      -- restores its own `pending_delete` — rather than reaching for the most
      -- recently deleted row, which may belong to another instance.
      if not state.deleted then
        return false
      end
      command("restore", { session = state.deleted })
      state.deleted = nil
      return true
    end

    local list = sessions()
    local items = build_model(list)
    if #items == 0 then
      return false
    end
    local at = widgets.clamp(state.cursor or 1, #items)
    local id = items[at].target or nil

    -- Moving the cursor republishes the selection here as well as in render,
    -- because Ctrl+J/K are global: with the column hidden (F9) or a terminal
    -- focused, this pane may not render again before the agent pane does.
    local function select_row(index)
      state.follow = nil
      state.cursor = index
      local target = items[index] and items[index].target or store.selected
      store.selected = target
      state.published = target
    end

    -- Actions, not chords. The kernel already resolved which key was pressed,
    -- so the capital-vs-shift encoding trap is its problem now, not ours.
    if action == "sessions.open" then
      -- The agent pane is what shows a session; focusing it is what "open" means
      -- here, exactly as v1's Enter moves focus to the terminal.
      if id then
        command("focus", { text = "agent" })
      end
    elseif action == "sessions.next" then
      select_row(move(items, at, 1))
    elseif action == "sessions.previous" then
      select_row(move(items, at, -1))
    elseif action == "sessions.first" then
      select_row(move(items, 0, 1))

    -- Every state change below is a COMMAND: accepted instantly, its effect
    -- appearing in a later snapshot. Nothing here waits for anything.
    elseif action == "sessions.delete" and id then
      if soft_delete() then
        -- Remembered for Ctrl+Z. Only the soft delete: a force-delete removed the
        -- worktree, so there is nothing an undo could put back.
        state.deleted = id
        command("delete", { session = id })
      else
        -- The switch is off, so this key deletes for real. v1 asks first when
        -- there is work to lose, because there is no undo to fall back on.
        local session = items[at].session
        delete_for_good(session, "Delete " .. (session.name or "this session") .. " for good?")
      end
    elseif action == "sessions.force_delete" and id then
      -- Destructive, and undone by nothing — but only worth a question when it
      -- would take work with it.
      local session = items[at].session
      delete_for_good(
        session,
        "Delete " .. (session.name or "this session") .. " and its worktree?"
      )
    elseif action == "sessions.restart" and id then
      command("restart", { session = id })
    elseif action == "sessions.fork" and id then
      -- v1 asked for the name first: `fork_active_session` prepared the spawn and
      -- opened its shared Session Name modal prefilled `<source>-fork`, so the
      -- fork was named before it existed and could be renamed on the spot. Forking
      -- silently was a v2 divergence, not a decision.
      --
      -- The naming UI lives in the creation float, so hand it the job through
      -- `store`, exactly as an irreversible change hands its question to
      -- `confirm`. This pane keeps owning what it knows (which session, and what
      -- the derived name is) and decides nothing about how it is asked.
      local source = items[at].session
      store.fork = {
        session = id,
        name = ((source and source.name) or "session") .. "-fork",
      }
    elseif action == "sessions.sync" and id then
      command("sync", { session = id })
    elseif action == "sessions.editor" and id then
      command("editor", { session = id })
    -- A move is computed over the RENDERED items and sent whole, so a root row
    -- drags its subtree and a group edge moves the group. `state.follow` keeps
    -- the cursor on the session rather than the row index, since the order it
    -- was pressed at lands a frame or two later.
    elseif action == "sessions.move_down" and id then
      local moved = move_block(items, at, true)
      if moved then
        state.follow = id
        persist_order(moved)
      end
    elseif action == "sessions.move_up" and id then
      local moved = move_block(items, at, false)
      if moved then
        state.follow = id
        persist_order(moved)
      end
    elseif action == "sessions.sort" then
      state.follow = id
      persist_order(sorted_within_groups(items))
    else
      return false
    end
    return true
  end,
}
