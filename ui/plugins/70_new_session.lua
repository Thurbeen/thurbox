-- The new-session flow.
--
-- v1 spent five modals, a 46-field wizard struct (`NewSessionWizardState`), three
-- background tasks and a generation counter on this. Here it is one file: a step
-- machine over reads the kernel publishes, ending in one `create` command.
--
-- The steps are v1's, in v1's order, and each is skipped exactly where v1 skips
-- it:
--
--   host  → repo → [base branch] → name → [branch name] → [agent] → create
--
-- `host` is skipped when nothing is configured, `base branch` and `branch name`
-- only appear when some repository is in worktree mode, and `agent` is skipped
-- when there is one or none. Nothing here waits: every listing, path check and
-- branch list is a READ the kernel serves from a worker
-- (`kernel::repos`), asked for by leaving a key in `store`.
--
-- WHY ONE PLUGIN AND NOT SIX. The kernel renders every floating plugin each
-- frame purely to discover it is not floating, so six closed modals would cost
-- six Lua calls a frame forever. They also share one thing — the pending choice
-- — which as six plugins would have to live in `store` as a protocol between
-- panes that only ever run in sequence.
--
-- WHERE A REFUSAL APPEARS. v1 puts "Not a git repo…" in the status bar. A plugin
-- cannot write the message band (it is kernel chrome), so a refusal this flow
-- makes itself is shown on its own footer row instead. A refusal from the
-- *kernel* — a path that does not exist, a folder with no repositories — comes
-- back as a failed command and the band reports it, as it does for every command.

local fuzzy = require("lib.fuzzy")
local modal = require("lib.modal")
local pathpicker = require("lib.pathpicker")
local repo_picker = require("lib.repo_picker")
local textinput = require("lib.textinput")
local theme = require("lib.theme")
local widgets = require("lib.widgets")

-- ── Reads, with their absences handled once ─────────────────────────────────

local function bookmarks()
  return (thurbox and thurbox.bookmarks) or {}
end

local function browse()
  return (thurbox and thurbox.browse) or {}
end

local function branches()
  return (thurbox and thurbox.branches) or {}
end

local function hosts()
  return (thurbox and thurbox.hosts) or {}
end

local function agents()
  return (thurbox and thurbox.agents) or {}
end

--- The spinner frame for this paint, from the flow's own clock.
local function spinner(flow)
  return theme.spinner_frame(flow.elapsed)
end

--- Is a repository-memory write still running?
---
--- v1 renders `Add Repo Path ⠋ checking…` while it validates a typed path on a
--- host. The same state is readable here: the command is in flight.
local function bookmark_pending()
  for _, item in ipairs((thurbox and thurbox.commands) or {}) do
    if item.kind == "bookmark" and item.phase ~= "failed" then
      return true
    end
  end
  return false
end

-- ── Flow state ─────────────────────────────────────────────────────────────
--
-- One table, read whole and written whole: `state` hands back a fresh Lua table
-- on every read, so a nested field mutated in place would not persist.

local function load()
  return state.flow
end

local function save(flow)
  state.flow = flow
end

--- A fresh flow. `step` is what "open" means — there is no separate flag for the
--- kernel and this to disagree about, exactly as floating itself works.
local function fresh()
  return {
    step = "host",
    host = "",
    host_index = 1,
    cursor = 1,
    focus = "list",
    selected = {},
    worktree = {},
    collapsed = {},
    input = textinput.new(""),
    search = nil,
    browse = false,
    browse_index = 1,
    select_newest = false,
    base = nil,
    branch_index = 1,
    name = textinput.new(""),
    branch = textinput.new(""),
    agent_index = 1,
    message = nil,
  }
end

--- Ask the kernel for what this step needs, and stop asking for the rest.
---
--- Written every frame because the want is what keeps the read served; clearing
--- the others is what makes a closed flow cost nothing.
local function ask(flow)
  if not flow then
    store.want_bookmarks = nil
    store.want_browse = nil
    store.want_branches = nil
    return
  end
  store.want_bookmarks = flow.host or ""

  -- The listing of the typed path's directory. Requested whether or not the
  -- dropdown is open, because the ghost completion is derived from it — which is
  -- how completion works for a remote target here and does not in v1.
  local typed = flow.step == "repo" and (flow.input.value or "") or ""
  if typed ~= "" then
    local dir = pathpicker.split_typed(typed)
    store.want_browse = (flow.host or "") .. "\0" .. dir
  else
    store.want_browse = nil
  end

  local primary = flow.primary
  if flow.step == "branch" and primary then
    store.want_branches = (flow.host or "") .. "\0" .. primary
  else
    store.want_branches = nil
  end
end

-- ── The repo step's row model ──────────────────────────────────────────────
--
-- The model itself is `lib.repo_picker` (matching on `lib.fuzzy`, like every
-- other picker) and the path-typing derivations are `lib.pathpicker`. The
-- wrappers below only gather what each needs from the flow and the published
-- reads, so every consumer in this file asks the same question the same way —
-- three derivations of "which entries" would eventually disagree about which
-- one `enter` picks.

local function rows_for(flow)
  local query = flow.search and (flow.search.value or "") or ""
  return repo_picker.rows(bookmarks().rows or {}, query, flow.collapsed)
end

--- The row the cursor is on, or nil when the list is empty.
local function current_row(flow)
  return repo_picker.current(rows_for(flow), flow.cursor)
end

local function chosen(flow)
  return repo_picker.chosen(bookmarks().rows or {}, flow.selected, flow.worktree)
end

local function browse_entries(flow)
  return pathpicker.entries(flow.input and flow.input.value or "", browse().entries or {})
end

local function suggestion_for(flow)
  return pathpicker.suggestion(flow.input and flow.input.value or "", browse().entries or {})
end

-- ── Rendering: the pieces every step shares ────────────────────────────────

--- The float's width, and the columns a list row inside it actually gets: the
--- modal's own border, then the border of the panel the list sits in.
local MODAL_COLS = 60
local ROW_COLS = MODAL_COLS - 4

--- The last component of a path, which is how people name a repository.
local function leaf(path)
  if not path or path == "" then
    return ""
  end
  return (path:gsub("/+$", ""):match("[^/]+$")) or path
end

--- What has been decided so far, for the title's trail.
---
--- A breadcrumb rather than `step 2 of 4`, because the flow is CONDITIONAL: the
--- host step is skipped when no host is configured, and the branch steps only
--- happen for a worktree. A count would have to lie about one of those.
local function trail(flow)
  if not flow then
    return ""
  end
  local parts = {}
  if (flow.host or "") ~= "" then
    parts[#parts + 1] = flow.host
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

--- The shared modal shell, with this flow's two constants folded in: every step
--- is `MODAL_COLS` wide (`cols`, in cells — every row here is truncated against
--- `ROW_COLS`), and every step's title carries the breadcrumb trail, muted, so
--- the step says where you are without spending a row on it.
local function frame(title, rows_height, children, flow)
  return modal.frame(title, {
    cols = MODAL_COLS,
    rows = rows_height,
    children = children,
    crumbs = trail(flow),
  })
end

--- A refusal this flow made itself, on its own row. Empty when there is none, so
--- the row is spent either way and the modal does not change height as messages
--- come and go.
local function message_row(flow)
  local text = flow.message
  return {
    type = "text",
    len = 1,
    text = { { { text = text and (" " .. text) or "", style = { fg = theme.bad } } } },
  }
end

--- v1's selector: `▸ ` on the current row, accent+bold, plain otherwise.
local function selector_rows(labels, index, height)
  local children = {}
  local first, last = widgets.window(#labels, height, index)
  for position = first, last do
    local selected = position == index
    children[#children + 1] = {
      type = "text",
      len = 1,
      text = {
        {
          {
            text = (selected and "▸ " or "  ") .. labels[position],
            style = selected and { fg = theme.accent, bold = true } or { fg = theme.text },
          },
        },
      },
      id = tostring(position),
      role = "row",
    }
  end
  return children
end

-- ── The steps ──────────────────────────────────────────────────────────────

--- v1's host picker: the local machine first, then each host with the detail
--- that tells two apart.
local function host_labels()
  local labels = { "local" }
  for _, host in ipairs(hosts()) do
    labels[#labels + 1] = host.name .. "  (" .. (host.detail or "") .. ")"
  end
  return labels
end

local function render_host(flow)
  local labels = host_labels()
  local height = #labels
  return frame("Run On", height + 4, {
    { type = "box", len = height, children = selector_rows(labels, flow.host_index, height) },
    message_row(flow),
    modal.footer({ { "j/k", "navigate" } }, "Select"),
  }, flow)
end

local REPO_LIST_MAX = 10
local BROWSE_MAX = 8

--- One row of the bookmark list. v1's `bookmark_item`, marker for marker.
local function repo_row(entry, selected, flow, is_cursor)
  local row = entry.row
  local style = is_cursor and { fg = theme.accent, bold = true } or { fg = theme.text }
  if row.is_parent then
    return {
      { text = (flow.collapsed[row.path] and "▸ " or "▾ ") .. row.path, style = style },
      { text = " (parent)", style = { fg = theme.muted } },
    }
  end

  local spans = {}
  if row.parent then
    spans[#spans + 1] = { text = "  ", style = style }
  end
  spans[#spans + 1] = { text = selected and "[x] " or "[ ] ", style = style }
  -- The label leads, because it is the answer to "what would picking this do";
  -- the path still follows it, muted, because it is the answer to "where".
  if row.label then
    spans[#spans + 1] = { text = row.label .. "  ", style = style }
  end
  -- Matched characters accented, as v1 accents them; the rest keeps the row's
  -- own style so the cursor row still reads as the cursor row. `fuzzy.spans`
  -- owns the character-boundary slicing, so a multi-byte hit cannot render as
  -- rubbish here any more than in the search strip.
  if #entry.matched > 0 then
    local hit = { fg = theme.accent_bright, bold = true }
    for _, span in ipairs(fuzzy.spans(row.path, entry.matched, style, hit)) do
      spans[#spans + 1] = span
    end
  else
    -- Middle-truncated: the leaf is what identifies a repository, and it is the
    -- half an end-truncation drops. `avail` is what the row has left after the
    -- indent, the checkbox, and any label ahead of the path.
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
  -- A known non-repository reads as a plain directory: selectable, but `w` will
  -- not take.
  if row.is_git == false then
    spans[#spans + 1] = { text = " (dir)", style = { fg = theme.muted } }
  end
  return spans
end

local function render_repo(flow)
  local entries = rows_for(flow)
  local total = #(bookmarks().rows or {})
  local visible = math.max(1, math.min(#entries, REPO_LIST_MAX))
  local searching = flow.search ~= nil
  local dropdown = flow.browse and browse() or nil

  local children = {}

  -- The search bar, above the list, only while searching — v1's layout exactly.
  if searching then
    children[#children + 1] = textinput.node(flow.search, {
      label = "Search (" .. #entries .. "/" .. total .. ")",
      focused = flow.focus == "search",
    })
  end

  local list = {}
  if #entries == 0 then
    list[1] = {
      type = "text",
      len = 1,
      text = {
        {
          {
            text = (searching and (flow.search.value ~= "")) and "  No matches"
              or "  No bookmarks — add via path input below",
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
      list[#list + 1] = {
        type = "text",
        len = 1,
        text = {
          repo_row(
            entry,
            flow.selected[path] == true,
            flow,
            position == cursor and flow.focus == "list"
          ),
        },
        id = path,
        role = "row",
      }
    end
  end

  local host_name = flow.host or ""
  children[#children + 1] = {
    type = "box",
    len = visible + 2,
    frame = {
      title = host_name ~= "" and (" Repos on " .. host_name .. " (" .. total .. ") ")
        or (" Repos (" .. total .. ") "),
      borders = "all",
      border_style = {
        fg = flow.focus == "list" and theme.border_focused or theme.border,
      },
    },
    children = list,
  }

  -- The path input, with the ghost completion sitting immediately after the
  -- typed text. The input node is sized to the value so the caret lands where
  -- the text ends and the ghost begins there rather than at the right edge.
  local suggestion = flow.suggestion or ""
  local label = "Add Repo Path"
  if bookmark_pending() then
    label = label .. " " .. spinner(flow) .. " checking…"
  end
  children[#children + 1] = {
    type = "box",
    axis = "horizontal",
    len = 3,
    frame = {
      title = " " .. label .. " ",
      borders = "all",
      border_style = {
        fg = flow.focus == "input" and theme.border_focused or theme.border,
      },
    },
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
        local spans = {
          {
            text = " " .. entry.name .. "/",
            style = selected and { fg = theme.accent, bold = true } or { fg = theme.text },
          },
        }
        if entry.is_git then
          spans[#spans + 1] = { text = " ●git", style = { fg = theme.accent } }
        end
        rows[#rows + 1] =
          { type = "text", len = 1, text = { spans }, id = entry.name, role = "row" }
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

  -- v1's footer, which says something different per focus — the keys really are
  -- different, and a hint that named them all would name most of them wrongly.
  local hints
  if flow.focus == "list" then
    hints = {
      { "j/k", "nav" },
      { "space", "toggle/fold" },
      { "w", "worktree" },
      { "/", "search" },
      { "d", "forget" },
      { "tab", "input" },
    }
  elseif flow.focus == "search" then
    hints = { { "enter", "keep filter" }, { "esc", "clear" } }
  elseif dropdown then
    hints = {
      { "↑/↓", "select" },
      { "enter", "open/pick" },
      { "s-tab", "list" },
      { "esc", "close" },
    }
  else
    hints = {
      { "tab", suggestion ~= "" and "complete" or "browse" },
      { "enter", "add repo" },
      { "ctrl+p", "import parent" },
      { "s-tab", "list" },
    }
  end
  children[#children + 1] = modal.footer(hints, "Done")

  -- The height is the sum of what was actually built, plus the two border rows.
  -- Deriving it from the children rather than recomputing the layout means the
  -- frame can never disagree with what is inside it — which is the bug a second
  -- arithmetic expression here would eventually be.
  local height = 2
  for _, child in ipairs(children) do
    height = height + (child.len or 1)
  end
  return frame("Select Repos", height, children, flow)
end

local function render_branch(flow)
  local list = branches()
  local names = list.list or {}
  if list.loading or #names == 0 then
    local text = list.error or (spinner(flow) .. " fetching and listing branches…")
    return frame("Base Branch", 5, {
      {
        type = "text",
        len = 1,
        text = {
          { { text = " " .. text, style = { fg = list.error and theme.bad or theme.muted } } },
        },
      },
      message_row(flow),
      modal.footer({ { "esc", "cancel" } }, "Select"),
    }, flow)
  end
  local height = math.min(#names, REPO_LIST_MAX)
  return frame("Base Branch", height + 4, {
    {
      type = "box",
      len = height,
      children = selector_rows(names, widgets.clamp(flow.branch_index, #names), height),
    },
    message_row(flow),
    modal.footer({ { "j/k", "navigate" } }, "Select"),
  }, flow)
end

local function render_field(title, label, field, flow, placeholder)
  return frame(title, 7, {
    textinput.node(field, {
      label = label,
      focused = true,
      placeholder = placeholder,
    }),
    message_row(flow),
    modal.footer({ { "enter", "confirm" }, { "esc", "cancel" } }, "OK"),
  }, flow)
end

local function render_agent(flow)
  -- v1's label: the name alone when the two are the same, else `name
  -- (command)`, so two entries wrapping the same CLI are distinguishable.
  local labels = {}
  for _, agent in ipairs(agents()) do
    labels[#labels + 1] = (agent.name == agent.command) and agent.name
      or (agent.name .. "  (" .. agent.command .. ")")
  end
  local height = math.max(1, math.min(#labels, REPO_LIST_MAX))
  return frame("Coding Agent", height + 4, {
    {
      type = "box",
      len = height,
      children = selector_rows(labels, widgets.clamp(flow.agent_index, #labels), height),
    },
    message_row(flow),
    modal.footer({ { "j/k", "navigate" } }, "Select"),
  })
end

-- ── Advancing ──────────────────────────────────────────────────────────────

--- v1's `session_name_to_branch`: lowercase alphanumerics, every run of spaces,
--- dashes and underscores collapsed to one dash, trimmed.
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

--- Where the flow goes after the repositories are chosen. v1's three-way split
--- in `submit_repo_picker`, with its ordering: worktrees first, then plain
--- directories, then nothing at all.
--- The name to use when the field is left empty: the repository's own.
---
--- Shown as a PLACEHOLDER rather than written into the field. Prefilling the value
--- would have been worse than the empty field it replaced: `{ value, cursor }` has
--- no selection, so there is no "typing replaces the suggestion" — the first
--- keystroke would have appended, and picking `thurbox` then typing `fix` would
--- have produced `thurboxfix`. As a placeholder the field is genuinely empty, so
--- typing behaves normally, and `Enter` on an untouched field is a valid answer
--- with the answer visible before you press it.
local function suggested_name(flow)
  -- A fork's default is the name it was handed, not the repository's leaf: the
  -- field starts prefilled, and clearing it must fall back to the same answer
  -- rather than to something about the directory.
  if flow and flow.fork then
    return flow.fork_name or ""
  end
  local name = leaf(flow and flow.primary)
  if name == "" or name == "~" then
    return ""
  end
  return name
end

--- A flow that exists only to name a fork.
---
--- v1's `fork_active_session` prepared the spawn and opened the SAME name modal the
--- creation flow uses, then spawned straight from it — the agent, the directory and
--- the worktrees all came from the source session, so there was nothing left to ask.
--- This is that, entered from `store.fork` rather than from a key: the sessions pane
--- knows which session and what the derived name is, and this knows how to ask.
local function fork_flow(handover)
  local flow = fresh()
  flow.step = "name"
  flow.fork = handover.session
  flow.fork_name = handover.name or ""
  textinput.set(flow.name, flow.fork_name)
  return flow
end

local function after_repos(flow)
  local worktrees, plain = chosen(flow)
  flow.extras = {}
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
  -- Nothing selected. v1 spawns in the home directory; a remote target has no
  -- local home to stand in for it and `create` needs a path on the machine the
  -- session will run on, so that one case asks for a repository instead.
  if (flow.host or "") ~= "" then
    flow.message = "Pick a repository on " .. flow.host
    return flow
  end
  flow.primary = "~"
  flow.step = "name"
  return flow
end

--- Issue the one command the whole flow exists to produce, and close.
local function commit(flow)
  if flow.fork then
    -- Everything else about a fork is the source session's, resolved by the
    -- kernel: the agent, the host, the directory, the shared worktrees and the
    -- parent link. The only thing this flow decides is the name.
    command("fork", { session = flow.fork, text = flow.name.value })
    save(nil)
    ask(nil)
    return
  end
  local picked = agents()[flow.agent_index]
  local agent = picked and picked.name or nil
  command("create", {
    text = flow.name.value,
    repo = flow.primary,
    branch = flow.base and flow.branch.value or nil,
    base = flow.base,
    agent = agent,
    host = (flow.host ~= "" and flow.host) or nil,
    extras = flow.extras or {},
  })
  save(nil)
  ask(nil)
end

--- After the name (and the branch name, in the worktree flow): the agent step,
--- or straight to creating when there is nothing to choose. v1's
--- `finish_prepare_spawn`.
local function after_name(flow)
  -- v1: "Fork flow — role already set, spawn directly." A fork inherits its
  -- source's agent, so offering the agent step would invite changing the one thing
  -- a fork cannot change.
  if flow.fork then
    commit(flow)
    return nil
  end
  local list = agents()
  if #list <= 1 then
    flow.agent_index = 1
    commit(flow)
    return nil
  end
  flow.step = "agent"
  flow.agent_index = 1
  for index, agent in ipairs(list) do
    if agent.name == (thurbox and thurbox.agent_default) then
      flow.agent_index = index
    end
  end
  return flow
end

-- ── The plugin ─────────────────────────────────────────────────────────────

--- Whether the browse dropdown is up and holding the keys.
---
--- It is a list of its own, so while it is open the step's list behind it must
--- not move as well.
local function dropdown_open(flow)
  return flow.browse == true and flow.focus == "input"
end

--- Move the selection of whatever step is showing.
---
--- One place rather than one per caller: the arrows and `j`/`k` must agree about
--- what "next" means on each of the four steps that are lists, and they reach it
--- by different routes — a declared action, and a raw key while a field has
--- focus.
local function move_selection(flow, step)
  if flow.step == "host" then
    flow.host_index = widgets.clamp(flow.host_index + step, #host_labels())
  elseif flow.step == "repo" then
    flow.cursor = widgets.clamp((flow.cursor or 1) + step, #rows_for(flow))
  elseif flow.step == "branch" then
    flow.branch_index = widgets.clamp(flow.branch_index + step, #(branches().list or {}))
  elseif flow.step == "agent" then
    flow.agent_index = widgets.clamp(flow.agent_index + step, #agents())
  end
end

return {
  name = "new_session",
  -- A slot the arrangement never places: this pane only ever floats, and a slot
  -- it could also occupy would make it an alternative to the terminal.
  slot = "float",
  order = 70,
  floats = true,
  -- Never in the focus ring, so `tab` cannot land on a closed flow — v1's
  -- pickers are modals for the same reason.
  focusable = false,
  -- Pure DESPITE the render's store/state writes, which is worth spelling out:
  -- a skipped render skips its writes, and that is safe here because every one
  -- of them is a function of inputs already in the cache key. The `ask` wants
  -- derive from the flow (state version), so a skipped render would only have
  -- re-written identical values — the kernel re-reads the persisted store keys
  -- each publish either way. The fork handover and `select_newest` consumption
  -- are transitions whose triggering writes (store.fork, the bookmark command
  -- landing) bump the state version or the epoch first, so the frame that must
  -- run always misses the cache. Floats render every frame even while closed;
  -- this is what makes the closed flow actually cost nothing.
  pure = true,

  keys = {
    -- v1's `Ctrl+N`, and like v1 NOT a passthrough chord: a focused agent does
    -- not need `ctrl+n`, and starting work is the one command that must be
    -- reachable from anywhere.
    {
      key = "ctrl+n",
      action = "new_session.open",
      desc = "new session",
      scope = "global",
      group = "Sessions",
    },
    -- The rest are plugin-scoped, so they fire only while the flow is up. Each
    -- is declared rather than merely handled, which is what puts it in help and
    -- makes it rebindable.
    { key = "j", action = "new_session.next", desc = "next choice", group = "New session" },
    { key = "k", action = "new_session.previous", desc = "previous choice", group = "New session" },
    -- Declared beside them because every other list here takes both, and a
    -- picker that only answers to `j`/`k` reads as broken to anyone who reaches
    -- for an arrow first.
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
      key = "w",
      action = "new_session.worktree",
      desc = "give a repository its own worktree",
      group = "New session",
    },
    {
      key = "d",
      action = "new_session.forget",
      desc = "forget a remembered repository",
      group = "New session",
    },
    {
      key = "/",
      action = "new_session.search",
      desc = "filter repositories",
      group = "New session",
    },
    {
      key = "ctrl+p",
      action = "new_session.import",
      desc = "import a folder of repositories",
      group = "New session",
    },
  },

  render = function(ctx)
    local flow = load()
    if not flow and type(store.fork) == "table" and store.fork.session then
      -- Taken here because this is where "is the float open" is decided, and the
      -- handover has to become a flow before that question is asked. Cleared as it
      -- is taken, so a cancelled fork does not reopen on the next frame.
      flow = fork_flow(store.fork)
      store.fork = nil
      save(flow)
    end
    ask(flow)
    if not flow then
      -- Not floating: the flow is closed, and a closed modal draws nothing.
      return { type = "text", text = "" }
    end

    -- Carried on the table the renderers already receive, rather than passed
    -- through five signatures, so a spinner can animate in any of them. Not
    -- saved: it belongs to this frame.
    flow.elapsed = ctx.elapsed
    flow.suggestion = flow.step == "repo" and suggestion_for(flow) or ""

    -- A path just added is selected, which is v1's select-or-add. The row cannot
    -- be found by name — the expansion of a `~` on a remote host happened on the
    -- worker — so it is found by RECENCY: memory is published most-recent-first
    -- and an add touches recency, so the row asked for is the first one
    -- (design.md D2). Consumed only once the write has landed and the list has
    -- been re-read, or it would select whatever was previously on top.
    if flow.select_newest and not bookmark_pending() and not bookmarks().loading then
      local first = (bookmarks().rows or {})[1]
      if first and not first.is_parent then
        flow.selected[first.path] = true
        flow.cursor = 1
      end
      flow.select_newest = nil
      save(flow)
    end

    if flow.step == "host" then
      return render_host(flow)
    elseif flow.step == "repo" then
      return render_repo(flow)
    elseif flow.step == "branch" then
      return render_branch(flow)
    elseif flow.step == "name" then
      return render_field("Session Name", "Name", flow.name, flow, suggested_name(flow))
    elseif flow.step == "worktree" then
      return render_field("Branch Name", "Branch", flow.branch, flow)
    end
    return render_agent(flow)
  end,

  --- Declared keys. Returning false hands the keystroke on to `on_key`, which is
  --- what lets `j` move the list in one focus and type a `j` in another.
  on_action = function(action)
    if action == "new_session.open" then
      -- A second press while the flow is up is a no-op rather than a reset:
      -- while it floats it takes every key, so `ctrl+n` here would otherwise be
      -- a way to lose a half-filled flow to a stray keystroke.
      if load() then
        return true
      end
      local flow = fresh()
      if #hosts() == 0 then
        -- v1 skips the question entirely rather than offering one answer.
        flow.step = "repo"
      end
      save(flow)
      ask(flow)
      return true
    end

    local flow = load()
    if not flow then
      return false
    end
    -- A text field owns every printable key while it has focus, so the letter
    -- actions below defer to it.
    local typing = flow.step == "repo" and flow.focus ~= "list"
      or flow.step == "name"
      or flow.step == "worktree"
    flow.message = nil

    if action == "new_session.next" or action == "new_session.previous" then
      -- `j`/`k` are letters, so a step with a text field has to be able to type
      -- them. The arrows are not, and are handled in `on_key` so they move the
      -- list even mid-typing — which is what v1's picker does.
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

    if action == "new_session.toggle" then
      if typing then
        return false
      end
      local entry = current_row(flow)
      if entry then
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
      if typing then
        return false
      end
      local entry = current_row(flow)
      if entry and not entry.row.is_parent then
        if entry.row.is_git == false then
          -- v1's refusal, in v1's words: worktree mode needs a repository, and
          -- the directory stays selectable without it.
          flow.message =
            "Not a git repo — worktree mode needs one (it can still be added as a plain dir)"
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
      if typing then
        return false
      end
      local entry = current_row(flow)
      if entry then
        if entry.row.parent then
          flow.message = "Child of a parent bookmark — delete the parent header instead"
        else
          command("bookmark", { host = flow.host, repo = entry.row.path, action = "remove" })
          flow.selected[entry.row.path] = nil
          flow.worktree[entry.row.path] = nil
        end
      end
      save(flow)
      return true
    elseif action == "new_session.search" then
      if typing then
        return false
      end
      flow.search = textinput.new("")
      flow.focus = "search"
      flow.cursor = 1
      save(flow)
      return true
    elseif action == "new_session.import" then
      -- Works from any focus, as v1's does, because it acts on the typed path.
      local path = (flow.input.value or ""):match("^%s*(.-)%s*$")
      if path == "" then
        flow.message = "Type a folder path, then ctrl+p to import its repos as a parent"
      else
        command("bookmark", { host = flow.host, repo = path, action = "parent" })
        textinput.clear(flow.input)
        flow.browse = false
        flow.focus = "list"
      end
      save(flow)
      return true
    end
    return false
  end,

  --- Everything a modal owns without declaring: confirm, cancel, move between
  --- halves, and typing. v1 keeps the same set fixed rather than rebindable.
  on_key = function(key)
    local flow = load()
    if not flow then
      return false
    end
    local name = key.key

    -- Arrows move the list even while a field has focus. They cannot be typed,
    -- so there is nothing to lose by claiming them — and the alternative is a
    -- path field you must leave before you can pick a row. The one exception is
    -- the browse dropdown, which is a list of its own.
    if (name == "down" or name == "up") and not dropdown_open(flow) then
      move_selection(flow, name == "down" and 1 or -1)
      save(flow)
      return true
    end

    -- Esc: out of the dropdown, out of the search, else out of the flow. Three
    -- levels, innermost first — v1's, and why a stray Esc never loses more than
    -- one thing at a time.
    if name == "esc" then
      if flow.step == "repo" and flow.browse then
        flow.browse = false
      elseif flow.step == "repo" and flow.search then
        flow.search = nil
        flow.focus = "list"
        flow.cursor = 1
      else
        save(nil)
        ask(nil)
        return true
      end
      save(flow)
      return true
    end

    flow.message = nil

    if flow.step == "host" then
      if name == "enter" then
        local index = flow.host_index
        flow.host = index > 1 and (hosts()[index - 1].backend or "") or ""
        flow.step = "repo"
        flow.cursor = 1
        -- Bookmarks are host-scoped, so the choices change with the host: an
        -- earlier host's selection must not carry over.
        flow.selected, flow.worktree, flow.collapsed = {}, {}, {}
        save(flow)
        ask(flow)
        return true
      end
      return false
    end

    if flow.step == "branch" then
      if name == "enter" then
        local names = branches().list or {}
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
        -- An untouched name field takes the suggestion the placeholder was
        -- showing, so `Enter` straight through is a valid answer and the answer
        -- was visible before the key was pressed. A branch has no such default:
        -- there is no obvious name for a branch that does not exist yet.
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
            -- The worktree flow names the branch next, prefilled from the
            -- session name exactly as v1 prefills it.
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
        commit(flow)
        return true
      end
      return false
    end

    -- The repo step: the list, the input, the search bar and the dropdown.
    if dropdown_open(flow) then
      local entries = browse_entries(flow)
      if name == "up" or name == "down" then
        local step = name == "down" and 1 or -1
        flow.browse_index = widgets.clamp((flow.browse_index or 1) + step, #entries)
        save(flow)
        return true
      elseif name == "backtab" then
        flow.browse = false
        flow.focus = "list"
        save(flow)
        return true
      elseif name == "enter" then
        local entry = entries[widgets.clamp(flow.browse_index, #entries)]
        if entry then
          local dir = (browse().dir or "")
          local joined = (dir == "/" and "/" or (dir .. "/")) .. entry.name
          if entry.is_git then
            -- Existence and git-ness were just observed, so this is a commit
            -- rather than a descent.
            command("bookmark", { host = flow.host, repo = joined, action = "add" })
            textinput.clear(flow.input)
            flow.browse = false
            flow.focus = "list"
            flow.select_newest = true
          else
            textinput.set(flow.input, joined .. "/")
            flow.browse_index = 1
          end
        end
        save(flow)
        ask(flow)
        return true
      end
      -- Anything else edits the path, and the listing follows the new directory.
    end

    if flow.focus == "search" then
      if name == "enter" then
        flow.focus = "list"
        save(flow)
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
        -- Derived here rather than read off the flow: the renderer's copy belongs
        -- to the frame it drew, and this pane does not stash it. (A render CAN
        -- write `state` — it persists whenever it happens — but deriving twice is
        -- simpler than keeping two places true.)
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
        flow.focus = "list"
        save(flow)
        return true
      elseif name == "enter" then
        local path = (flow.input.value or ""):match("^%s*(.-)%s*$")
        if path ~= "" then
          -- Validated on the target machine, not here: a missing path is refused
          -- by the command and reported, with the text left to be corrected.
          command("bookmark", { host = flow.host, repo = path, action = "add" })
          textinput.clear(flow.input)
          flow.browse = false
          flow.select_newest = true
        end
        save(flow)
        ask(flow)
        return true
      end
      if textinput.key(flow.input, key) then
        -- The visible set just changed, so the cursor starts again rather than
        -- pointing at whatever now occupies its old position. v1 resets it too.
        flow.browse_index = 1
        save(flow)
        ask(flow)
        return true
      end
      return false
    end

    -- The list.
    if name == "tab" then
      flow.focus = "input"
      save(flow)
      return true
    elseif name == "enter" then
      save(after_repos(flow))
      ask(load())
      return true
    elseif name == "up" or name == "down" then
      local count = math.max(1, #rows_for(flow))
      flow.cursor = math.max(1, math.min((flow.cursor or 1) + (name == "down" and 1 or -1), count))
      save(flow)
      return true
    end
    return false
  end,

  --- A click selects the row it landed on, exactly as `j`/`k` would. Rows carry
  --- their path rather than an index, so a list that changed between the paint
  --- and the press still selects what was pointed at.
  on_click = function(hit)
    local flow = load()
    if not flow or not hit.id then
      return false
    end
    if flow.step == "repo" then
      local index = widgets.index_of(rows_for(flow), hit.id, function(entry)
        return entry.row.path
      end)
      if not index then
        return false
      end
      flow.cursor = index
      flow.focus = "list"
      save(flow)
      return true
    end
    local index = tonumber(hit.id)
    if not index then
      return false
    end
    if flow.step == "host" then
      flow.host_index = index
    elseif flow.step == "branch" then
      flow.branch_index = index
    elseif flow.step == "agent" then
      flow.agent_index = index
    end
    save(flow)
    return true
  end,
}
