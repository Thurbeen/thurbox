-- The new-session flow.
--
-- v1 spent five modals, a 46-field wizard struct (`NewSessionWizardState`), three
-- background tasks and a generation counter on this. Here it is one file: a step
-- machine over reads the kernel publishes, ending in one `create` command.
--
-- A session is a Fleet Task (ADR-41), and the steps ask for what a Task takes:
--
--   host  → repo → [base branch] → name → [branch name] → [agent] → create
--
-- plus a detour off the repo step for a typed path that does not exist yet:
--
--   repo → new folder (git init / leave it empty) → repo
--                     (clone into it)         → clone URL → repo
--
-- These are v2's steps, in v2's order. `host` is always asked: this machine
-- is a choice at once, and the hosts Fleet names join the open step when they
-- arrive. The repo step gathers repositories, each ticked (`space`) and each
-- given a worktree of its own or not (`alt+w`); a repository's existing
-- worktrees open under it (`→`), and one picked runs as it is. `base branch`
-- and `branch name` only appear when some repository has a worktree, and
-- `agent` is skipped when there is one harness or none. A worktree is made on
-- this machine only: thurbox reads no other host's branches, so a path on
-- another host runs as it is. Each repository is one URL of the Task's
-- workspace, the first where the agent starts, and the runtime makes every
-- worktree from its `git+file` URL (ADR-41). Nothing here waits: every
-- listing, path check and branch list is a READ the kernel serves from a
-- worker (`kernel::repos`), asked for by leaving a key in `store`.
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
local hover = require("lib.hover")
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

--- Every host Fleet defines, `local` first when it does; `nil` until Fleet
--- has answered, which is pending, not empty.
local function hosts()
  return thurbox and thurbox.hosts
end

local function agents()
  return (thurbox and thurbox.agents) or {}
end

--- The branches of the repository `store.want_branches` names, or `nil`
--- while they are not known for `repo`: pending, not empty.
local function repo_branches(repo)
  local read = thurbox and thurbox.repo_branches
  if not read or read.repo ~= repo or read.loading then
    return nil
  end
  return read
end

--- Whether Fleet has said which agents it has. Until it has, the list is
--- pending, not empty, and nothing is created without one.
local function agents_known()
  return thurbox ~= nil and thurbox.agents ~= nil
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

--- The repository-memory writes that have failed and are still being
--- reported: their path (the command's `subject`, as issued) keyed by id as a
--- string.
local function bookmark_failures()
  local failed = {}
  for _, item in ipairs((thurbox and thurbox.commands) or {}) do
    if item.kind == "bookmark" and item.phase == "failed" then
      failed[tostring(item.id)] = item.subject or ""
    end
  end
  return failed
end

-- ── Flow state ─────────────────────────────────────────────────────────────
--
-- One table, read whole and written whole: `state` hands back a fresh Lua table
-- on every read, so a nested field mutated in place would not persist.

local function load()
  return state.flow
end

--- Assigned once the repo step's row model exists below. Declared here because
--- `save` is the one choke point every flow mutation passes through, which makes
--- it the only place "which repo is the cursor on" can be answered without each
--- of the eight cursor-moving sites remembering to ask.
local track_worktrees

local function save(flow)
  if track_worktrees then
    track_worktrees(flow)
  end
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
    -- The repo step opens on its search, not on the list: the first thing a
    -- hand does there is type part of a repository's name. The other focus is
    -- `input`, the path field.
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
    -- The repo whose existing worktrees are showing as child rows, and the one
    -- chosen to open (`{ path, branch }`) once one has been.
    wt_repo = nil,
    open_worktree = nil,
    name = textinput.new(""),
    branch = textinput.new(""),
    agent_index = 1,
    -- The detour for a typed path that does not exist: where, and which of
    -- `FOLDER_CHOICES` the cursor is on.
    new_path = nil,
    folder_index = 1,
    url = textinput.new(""),
    -- What the path field's spinner says while a repository-memory write is in
    -- flight: `checking…` for an add, `cloning…` for a clone, which takes long
    -- enough that "checking" would read as stuck.
    pending_label = nil,
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
  -- The branches of the first worktree repository, while the base branch is
  -- asked; on the repo step, the expanded repository's, whose read carries
  -- its existing worktrees. Local only: a worktree is never offered for a
  -- path on another host.
  if flow.step == "branch" then
    store.want_branches = flow.primary
  elseif flow.step == "repo" and flow.wt_repo then
    store.want_branches = flow.wt_repo
  else
    store.want_branches = nil
  end

  -- The listing of the typed path's directory. Requested whether or not the
  -- dropdown is open, because the ghost completion is derived from it — which is
  -- how completion works for a remote target here and does not in v1.
  --
  -- An EMPTY field still names a directory: `split_typed` answers `~` for it,
  -- exactly as it does for a bare `~`. Asking only once something had been typed
  -- is what made `tab` on a fresh field open a dropdown nothing could ever fill —
  -- and `tab` on a fresh field is how browsing starts, so it read as "tab no
  -- longer browses". The want is still dropped the moment the dropdown closes, so
  -- a flow that is only picking from memory asks for no listing at all.
  --
  -- Only while the field has focus: it holds its starting directory the whole
  -- time now, and a flow picking from memory must not pay for a listing.
  local typed = flow.step == "repo" and (flow.input.value or "") or ""
  if flow.step == "repo" and flow.focus == "input" and (typed ~= "" or flow.browse) then
    local dir = pathpicker.split_typed(typed)
    store.want_browse = (flow.host or "") .. "\0" .. dir
  else
    store.want_browse = nil
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
  local query = flow.search.value or ""
  local entries = repo_picker.rows(bookmarks().rows or {}, query, flow.collapsed)
  local read = flow.wt_repo and repo_branches(flow.wt_repo)
  local published = read and { repo = read.repo, list = read.worktrees } or nil
  return repo_picker.with_worktrees(entries, flow.wt_repo, published)
end

--- The row the cursor is on, or nil when the list is empty.
local function current_row(flow)
  return repo_picker.current(rows_for(flow), flow.cursor)
end

--- Keep an explicit expansion while moving between its repo and child rows.
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

--- The repository the cursor stands in for when nothing is ticked, or nil.
---
--- "Ticked, else the cursor row": type part of a name and confirm, with no
--- `space` in between. A folder header is not a repository, and a worktree row
--- is opened rather than gathered, so neither stands in.
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

--- A repository-memory write has been issued: tick the row it lands as (see
--- `select_newest` in the render), and clear a search typed before the path
--- was — one the new row need not match, which would hide it once ticked.
---
--- `path` is the path exactly as issued, and the failures already on record
--- are noted too: a new failure for that path is this write's, and a write that
--- never landed has no row to tick (see the render). Another write failing
--- meanwhile says nothing about this one.
local function await_new_row(flow, path)
  flow.select_newest = true
  flow.awaiting_path = path
  flow.failures_before = bookmark_failures()
  textinput.clear(flow.search)
  flow.cursor = 1
end

--- Move focus into the path field, filling an empty one with where a new path
--- most likely goes (`pathpicker.start`). The fill is remembered so it can be
--- told apart from something typed: until it is edited it is no choice at all.
local function enter_path_field(flow)
  flow.focus = "input"
  if (flow.input.value or "") == "" then
    flow.prefill = pathpicker.start(bookmarks().rows or {})
    textinput.set(flow.input, flow.prefill)
  end
end

--- Is the path field still holding only its starting directory?
local function untouched(flow)
  return flow.prefill ~= nil and flow.input.value == flow.prefill
end

--- The typed path with its trailing slashes gone, which is the folder it names.
local function typed_target(flow)
  local value = (flow.input.value or ""):match("^%s*(.-)%s*$")
  return (value:gsub("(.)/+$", "%1"))
end

--- Does the path field name a folder that is known not to exist?
---
--- Known from the listing the field already asks for — the parent's — and only
--- once it has answered for that very directory: until then the path may well
--- exist, and `enter` stays the add the kernel checks. A listing that failed
--- means a parent is missing too, which `mkdir -p` makes, so it counts.
local function names_missing_folder(flow)
  if untouched(flow) then
    return false
  end
  local target = typed_target(flow)
  if target == "" or target == "/" or target == "~" then
    return false
  end
  local dir, leaf = pathpicker.split_typed(target)
  local listing = browse()
  if listing.dir ~= dir or (listing.host or "") ~= (flow.host or "") or listing.loading then
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
--- branch steps only happen for a worktree, and the agent step is skipped when
--- there is nothing to choose. A count would have to lie about those.
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

--- The band a row wears under the pointer, which the row under the cursor
--- never does: it already reads as the cursor, and a band on it would say
--- nothing a click there could change.
local function row_hover(id, is_cursor)
  if not is_cursor and hover.id(id) then
    return hover.row_style()
  end
  return nil
end

--- v1's selector: `▸ ` on the current row, accent+bold, plain otherwise.
local function selector_rows(labels, index, height)
  local children = {}
  local first, last = widgets.window(#labels, height, index)
  for position = first, last do
    local selected = position == index
    local id = tostring(position)
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
      style = row_hover(id, selected),
      id = id,
      role = "row",
    }
  end
  return children
end

-- ── The steps ──────────────────────────────────────────────────────────────

--- The host picker: each host Fleet defines, with the detail that tells two
--- apart. Fleet's `local` is offered only when Fleet defines it.
local function host_labels()
  local labels = {}
  for _, host in ipairs(hosts() or {}) do
    labels[#labels + 1] = host.name .. "  (" .. (host.detail or "") .. ")"
  end
  return labels
end

local function render_host(flow)
  local labels = host_labels()
  if hosts() == nil then
    labels = { "Asking Fleet for its hosts…" }
  end
  -- Fleet's answer replaces the list under an open step, and can shorten it.
  if flow.host_index > #labels then
    flow.host_index = math.max(#labels, 1)
    save(flow)
  end
  local height = #labels
  return frame("Run On", height + 4, {
    { type = "box", len = height, children = selector_rows(labels, flow.host_index, height) },
    message_row(flow),
    modal.footer({ { "j/k", "navigate" } }, "Select"),
  }, flow)
end

local REPO_LIST_MAX = 10
local BROWSE_MAX = 8

-- The repo step's click targets. Every kind carries its own prefix, so no
-- remembered path — a relative one can be any string — can be read as a field
-- or a browsed folder, nor a folder's name as a bookmark.
local SEARCH_FIELD = "field:search"
local PATH_FIELD = "field:input"
local BROWSE_ROW = "browse:"
local REPO_ROW = "repo:"

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

  -- An existing worktree: not a thing to tick, a thing to open. So no checkbox,
  -- and the branch is what identifies it — the directory name is usually a
  -- shortened form of the branch, and the branch is what the work is on.
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

--- Does `enter` on the name spawn straight away, with nothing left to ask?
---
--- One agent is not a question (see `after_name`, which is what actually
--- decides it). Shared by the name field's pill and `after_name` so they cannot
--- drift apart.
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

  -- The search bar is always there, above the list it filters: it is where the
  -- keys go from the moment the flow opens.
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

  local host_name = flow.host or ""
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

  -- The path input, with the ghost completion sitting immediately after the
  -- typed text. The input node is sized to the value so the caret lands where
  -- the text ends and the ghost begins there rather than at the right edge.
  local suggestion = flow.suggestion or ""
  local label = "Add Repo Path"
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
    -- The whole framed field is the target, not the input inside it: the input
    -- is sized to its text, so a press on the empty rest of the field would
    -- otherwise land on nothing.
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

  -- v1's footer, which says something different per focus — the keys really are
  -- different, and a hint that named them all would name most of them wrongly.
  -- The two pills are subject to that as much as the hints: they REPLAY `enter`
  -- and `esc`, so a fixed pair of labels ("Done", "Cancel") would name four
  -- different actions each, and would name them wrongly in three focuses out of
  -- four. `enter` in particular never finishes this flow: it adds the typed
  -- path, or picks a browsed one, or keeps the filter, or moves on to the next
  -- question.
  --
  -- Which is why `enter` is not in the hints as well: the pill beside them now
  -- carries its action word for word, and the strip is 58 columns wide — the
  -- repetition cost `alt+p` its description entirely, and that one is the only
  -- place a folder import is offered at all.
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
      -- An existing worktree is not a selection to gather but a thing to open —
      -- but `enter` only spawns straight from here when nothing is left to ask
      -- (see `spawns_directly`); otherwise it advances to the agent step same as
      -- every other row.
      primary = spawns_directly() and "Open" or "Next"
    else
      -- `enter` carries the TICKED rows, not the row under the cursor, and with
      -- none ticked on a host `after_repos` refuses and stays here: a remote
      -- target has no local home to stand in for a repository. So there is
      -- nothing to advance to and nothing is offered — which on a host whose
      -- memory is still empty is the whole of that step until a path is added.
      -- Spelled as an `if`: `cond and nil or "Next"` is always "Next", because
      -- `and` yielding nil falls through to the `or`.
      local worktrees, plain = chosen(flow)
      local nothing_to_carry = #worktrees == 0 and #plain == 0 and not cursor_stand_in(flow)
      if nothing_to_carry and (flow.host or "") ~= "" then
        primary = nil
      else
        primary = "Next"
      end
    end
    -- `esc` clears a typed query before it closes anything.
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
      -- The portable spelling; `ctrl+enter` does the same where the terminal
      -- can send it, and help lists both.
      { "alt+⏎", "next" },
    }
    -- An empty field is nothing to add — including straight after an add, which
    -- clears it and leaves the focus here so several paths can be typed in a
    -- row. Trimmed as `enter` trims it, so a field holding only spaces reads as
    -- the nothing it is.
    -- The untouched starting directory is not a choice either.
    local typed = (flow.input.value or ""):match("^%s*(.-)%s*$")
    if typed == "" or untouched(flow) then
      primary = nil
    elseif names_missing_folder(flow) then
      primary = "Create folder"
    else
      primary = "Add repo"
    end
  end
  -- Stacked: this step has more keys than one row can name beside its pills,
  -- and a hint cut off at the pill is a key nobody learns (forget was).
  if flow.focus == "search" then
    children[#children + 1] = {
      type = "text",
      len = 1,
      text = "← close worktrees  → expand worktrees",
      style = { fg = theme.muted },
    }
  end
  children[#children + 1] = modal.footer(hints, primary, { cancel = cancel, stack = true })

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

--- What can go into a folder made for a path that did not exist, in the order
--- offered: the `bookmark` action each one issues, and how it reads.
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
    local text = (read and read.error) or (spinner(flow) .. " fetching and listing branches…")
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
      -- No confirm pill: there is nothing to select yet, and one offered here
      -- would be a button that does nothing when pressed.
      modal.footer({ { "esc", "cancel" } }, nil),
    }, flow)
  end
  -- The cursor starts on the branch a new worktree starts from by default, the
  -- first time the list is there to put it on.
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
  -- A fetch that failed leaves the refs as they were: say so, or `origin/main`
  -- reads as the upstream's tip when it is only the last one fetched.
  if read.fetch_error then
    rows[1] = {
      type = "text",
      len = 1,
      text = {
        {
          {
            text = widgets.middle_truncate(" not fetched: " .. read.fetch_error, ROW_COLS),
            style = { fg = theme.bad },
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
  return frame("Base Branch", height + 4 + (read.fetch_error and 1 or 0), rows, flow)
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
    -- No pill until there is a URL: `enter` would only refuse.
    modal.footer({ { "esc", "back" } }, url ~= "" and "Clone" or nil, { cancel = "Back" }),
  }, flow)
end

--- Does `enter` on this field end the flow?
---
--- The name is the last question only sometimes: the worktree flow asks for a
--- branch name after it, and a name followed by a choice of agent is not the
--- end either. One agent is not a question, so it spawns straight from the
--- field. Mirrors `after_name`, which is what actually decides it.
local function field_creates(flow)
  if flow.step == "name" and flow.base then
    return false
  end
  return spawns_directly()
end

local function render_field(title, label, field, flow, placeholder)
  -- What `enter` would actually take: the typed value, or the placeholder
  -- standing in for it. The name step's suggestion is a real answer — `on_key`
  -- takes it from an untouched field — while the branch step has no such
  -- default, and neither has a repository whose leaf is no kind of name (the
  -- home directory the flow falls back to). When that resolves to nothing,
  -- `enter` is refused with "cannot be empty", so no pill is offered until
  -- there is something to confirm. The field is where that gets fixed, and it
  -- already has the caret in it.
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
      -- No pill: there is nothing to choose yet.
      modal.footer({ { "esc", "cancel" } }, nil),
    }, flow)
  end
  -- The harnesses Fleet has profiles for, by name.
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
    -- The last question: `enter` here spawns the session rather than merely
    -- settling the agent, and the pill that replays it says so.
    modal.footer({ { "j/k", "navigate" } }, "Create"),
  })
end

-- ── Advancing ──────────────────────────────────────────────────────────────

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
  local name = leaf(flow and flow.primary)
  if name == "" or name == "~" then
    return ""
  end
  return name
end

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
--- directories, then nothing at all. The first of them is where the agent
--- starts; the rest are the session's other workspace URLs, in that order.
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
  local picked = agents()[flow.agent_index]
  -- Opening an existing worktree: it runs as it is, on the branch already
  -- checked out there, named after its directory.
  local open = flow.open_worktree
  local worktree = flow.base ~= nil and not open
  command("create", {
    text = (open and leaf(open.path)) or flow.name.value,
    repo = (open and open.path) or flow.primary,
    worktree = worktree,
    branch = worktree and flow.branch.value or nil,
    base = worktree and flow.base or nil,
    agent = picked and picked.name or nil,
    host = (flow.host ~= "" and flow.host) or nil,
    extras = (not open and flow.extras) or {},
  })
  save(nil)
  ask(nil)
end

--- After the name (and the branch name, in the worktree flow): the agent step,
--- or straight to creating when there is nothing to choose. v1's
--- `finish_prepare_spawn`.
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

--- Carry the repo step on to the next question: the ticked rows, else the row
--- under the cursor.
---
--- An existing worktree under the cursor is not a selection to gather: it names
--- its own repo, branch and directory, so there is nothing left to ask — unless
--- rows are ticked, which are what the reader has been gathering.
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
  -- `create` is issued from here. Granted without asking while this file is
  -- the one thurbox ships.
  capabilities = { "sessions" },
  ui_state = function()
    local flow = load()
    if not flow then
      return { open = false }
    end
    local selection = flow.cursor or 1
    if flow.step == "host" then
      selection = flow.host_index
    elseif flow.step == "agent" then
      selection = flow.agent_index
    elseif flow.step == "new_folder" then
      selection = flow.folder_index
    elseif flow.step == "branch" then
      selection = flow.branch_index
    end
    return { open = true, step = flow.step, selection = selection }
  end,
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
  -- each publish either way. The `select_newest` consumption is a transition
  -- whose triggering write (the bookmark command landing) bumps the epoch
  -- first, so the frame that must
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
    -- Chords rather than letters: the repo step types every letter into its
    -- search, so a bare `w` or `d` is part of a repository's name there.
    {
      key = "alt+w",
      action = "new_session.worktree",
      desc = "give a repository its own worktree",
      group = "New session",
    },
    {
      -- Not `ctrl+d`: that is the global delete-session chord, and a float's
      -- claim on a global chord is a conflict the registry reports.
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
    -- Confirm the repo step from any focus — the path field included — with the
    -- ticked rows, else the cursor row. `ctrl+enter` reaches a terminal only
    -- under the kitty keyboard protocol; `alt+enter` is the same action for
    -- every other terminal.
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
      -- `alt`, not `ctrl`: `ctrl+p` is the command palette everywhere, and a
      -- float's own claim on a global chord is a conflict the registry reports.
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
      -- Not floating: the flow is closed, and a closed modal draws nothing.
      return { type = "text", text = "" }
    end

    -- Carried on the table the renderers already receive, rather than passed
    -- through five signatures, so a spinner can animate in any of them. Not
    -- saved: it belongs to this frame.
    flow.elapsed = ctx.elapsed
    flow.suggestion = flow.step == "repo" and flow.focus == "input" and suggestion_for(flow) or ""

    -- A path just added is selected, which is v1's select-or-add. The row cannot
    -- be found by name — the expansion of a `~` on a remote host happened on the
    -- worker — so it is found by RECENCY: memory is published most-recent-first
    -- and an add touches recency, so the row asked for is the newest one.
    -- Newest is not the same as first: the kernel offers the
    -- interface directory ahead of memory, and taking row 1 selected THAT for
    -- every repository added. Consumed only once the write has landed and the
    -- list has been re-read, or it would select whatever was previously on top.
    if flow.select_newest and not bookmark_pending() and not bookmarks().loading then
      -- A failure that was not on record when the write was issued is this
      -- write's: the newest row is then whatever was newest before it, and
      -- ticking that would pick a repository nobody chose.
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
        -- The cursor follows the selection rather than resetting to the top, for
        -- the same reason: the top row is no longer the row just added.
        flow.cursor = repo_picker.index_of(rows_for(flow), newest.path) or 1
      end
      flow.select_newest = nil
      flow.awaiting_path = nil
      flow.failures_before = nil
      flow.pending_label = nil
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
    elseif flow.step == "new_folder" then
      return render_new_folder(flow)
    elseif flow.step == "clone" then
      return render_clone(flow)
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
      save(flow)
      ask(flow)
      return true
    end

    local flow = load()
    if not flow then
      return false
    end
    -- Every step with a text field types the letters `j`/`k`; the repo step
    -- always has one focused (its search or its path).
    local typing = flow.step == "repo"
      or flow.step == "name"
      or flow.step == "worktree"
      or flow.step == "clone"
    local in_search = flow.step == "repo" and flow.focus == "search"
    flow.message = nil

    if action == "new_session.next" or action == "new_session.previous" then
      -- The arrows are not letters, and are handled in `on_key` so they move the
      -- list even mid-typing — which is what v1's picker does.
      if typing then
        return false
      end
      move_selection(flow, action == "new_session.next" and 1 or -1)
      save(flow)
      return true
    end

    if flow.step ~= "repo" then
      -- Elsewhere the chord is still an `enter`: `on_key` matches the key name
      -- and ignores the modifier.
      return false
    end

    if action == "new_session.confirm" then
      flow.browse = false
      confirm_repos(flow)
      return true
    elseif action == "new_session.toggle" then
      -- A space is a character the path field is entitled to; the search gives
      -- it up, because no repository is found by typing one.
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
          -- v1's refusal, in v1's words: worktree mode needs a repository, and
          -- the directory stays selectable without it.
          flow.message =
            "Not a git repo — worktree mode needs one (it can still be added as a plain dir)"
        elseif (flow.host or "") ~= "" then
          -- thurbox reads no other machine's branches, so it cannot ask what a
          -- worktree there starts from: the repository runs as it is.
          flow.message = "Worktrees are made on this machine only — it runs as it is"
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
      -- Only from the search: in the path field `alt+d` and `delete` are the
      -- edits every field takes. In the search they still delete forward while
      -- there is text ahead of the caret, and mean "forget" only past the end,
      -- where they would otherwise do nothing — the way readline's `ctrl+d`
      -- deletes a character or, on an empty line, ends the input.
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
      -- Works from any focus, as v1's does, because it acts on the typed path.
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
      elseif flow.step == "repo" and flow.focus == "search" and flow.search.value ~= "" then
        textinput.clear(flow.search)
        flow.cursor = 1
      elseif flow.step == "new_folder" then
        -- Back to the path, still typed, so a slip in the name is one edit away.
        flow.step = "repo"
        flow.focus = "input"
      elseif flow.step == "clone" then
        flow.step = "new_folder"
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

    if flow.step == "host" then
      if name == "enter" then
        local chosen = (hosts() or {})[flow.host_index]
        if not chosen then
          -- Fleet has not answered yet, or defines no host: nothing to run on.
          return true
        end
        flow.host = chosen.backend or ""
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
            -- session name exactly as v1 prefills it. A branch that already
            -- exists is checked out rather than made.
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
          -- The URL is still to be asked; nothing is made until it is.
          flow.step = "clone"
          textinput.clear(flow.url)
          save(flow)
          return true
        end
        command("bookmark", { host = flow.host, repo = flow.new_path, action = choice.action })
        -- Back on the repositories with the search focused, so the new row —
        -- picked once it lands, as a typed path's is — goes on with one `enter`.
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
        flow.focus = "search"
        save(flow)
        return true
      elseif name == "enter" or name == "tab" then
        local entry = entries[widgets.clamp(flow.browse_index, #entries)]
        if entry then
          local dir = (browse().dir or "")
          local joined = (dir == "/" and "/" or (dir .. "/")) .. entry.name
          if name == "enter" then
            -- A session can use any directory, a git repository or not.
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
      -- Anything else edits the path, and the listing follows the new directory.
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
          -- Local only, as every worktree is: thurbox reads no other machine.
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
        -- The visible set just changed, so the cursor starts again at the best
        -- match rather than pointing at whatever now occupies its old position.
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
        flow.focus = "search"
        save(flow)
        return true
      elseif name == "enter" then
        local path = (flow.input.value or ""):match("^%s*(.-)%s*$")
        if names_missing_folder(flow) then
          -- Nothing is made yet: what goes into it is the next question.
          flow.new_path = typed_target(flow)
          flow.folder_index = 1
          flow.step = "new_folder"
          flow.browse = false
        elseif path ~= "" and not untouched(flow) then
          -- Validated on the target machine, not here: a missing path is refused
          -- by the command and reported, with the text left to be corrected.
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
      -- `/` or `~` typed over the untouched start is a path of its own, the way
      -- an address bar takes a new address; anything else is a name under it.
      if untouched(flow) and (key.char == "/" or key.char == "~") then
        textinput.clear(flow.input)
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
    return false
  end,

  --- A click selects the row it landed on, exactly as `j`/`k` would. Rows carry
  --- their path rather than an index, so a list that changed between the paint
  --- and the press still selects what was pointed at.
  ---
  --- On the repo step a press on a field moves the focus there, as `tab`
  --- (into the path) and `shift+tab` (back to the search) do, and a press on a
  --- browsed folder selects it as the arrows do. A press on the field that
  --- already has focus changes nothing: `tab` there completes or browses, and
  --- a click is not asking for either.
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
    if flow.step == "host" then
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
