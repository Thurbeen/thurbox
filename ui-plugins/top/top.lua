-- CPU and memory, from `top`, as gauges.
--
-- NOT bundled. Install it into your interface with:
--
--     thurbox-cli plugin install top
--
-- Then trust it: settings (`Ctrl+,`) → Interface (`]`) → the row → `t`. Until you
-- do, `run` is **not in this plugin's environment** — not a function that returns
-- an error, absent — so `render` checks for it rather than assuming, and draws the
-- reason instead of a blank pane.
--
-- The property worth noticing: it reads the machine the SELECTED SESSION runs on.
-- `run` executes in that session's directory, on that session's host, so moving
-- the cursor onto a session running over SSH shows the load on the remote box
-- with nothing here knowing what SSH is.
--
-- Parsing note. `top` has two dialects and this reads both:
--
--   procps (Linux)  %Cpu(s): 12.1 us,  2.3 sy, ...  64.2 id, ...
--                   MiB Mem :  32017.1 total,  1086.1 free,  15603.1 used, ...
--   BSD (macOS)     CPU usage: 5.9% user, 2.0% sys, 92.1% idle
--                   PhysMem: 8000M used (1200M wired), 4000M unused.
--
-- Idle is what both report, so CPU is `100 - idle` rather than the sum of the
-- busy fields — which differ per dialect and do not always add up.

local theme = require("lib.theme")
local widgets = require("lib.widgets")

--- Rows of the process table to show under the gauges.
local PROCESSES = 6

--- Seconds an answer stays fresh. `top` is cheap but not free, and a gauge that
--- moves once a second reads as live without costing a process per frame.
local TTL = 2

--- One `top` snapshot, bounded: the whole table would be hundreds of lines and
--- the kernel would truncate it anyway.
local COMMAND = "top -bn1 2>/dev/null | head -"
  .. tostring(7 + PROCESSES)
  .. " || top -l 1 -n "
  .. tostring(PROCESSES)

--- Colour by pressure, not by value: the same 80% means the same thing whatever
--- is being measured, and the roles keep it right in every palette.
local function pressure(ratio)
  if ratio >= 0.85 then
    return theme.bad
  elseif ratio >= 0.6 then
    return theme.warn
  end
  return theme.ok
end

--- `12.1` out of `%Cpu(s): 12.1 us, ... 64.2 id` — or nil when the line is not
--- there, which is how the BSD branch below gets its turn.
local function number_before(text, unit)
  local found = text:match("([%d%.]+)%s*" .. unit)
  return found and tonumber(found) or nil
end

--- CPU busy fraction, 0..1.
local function cpu_of(out)
  -- procps: the idle field is `id`. BSD: `92.1% idle`.
  local idle = number_before(out, "id[,%s]") or number_before(out, "%%%s*idle")
  if not idle then
    return nil
  end
  return math.max(0, math.min((100 - idle) / 100, 1))
end

--- Memory as `used, total` in MiB, or nil when neither dialect matched.
local function mem_of(out)
  -- procps states both, in whatever unit it chose (`MiB Mem :`).
  local total, used = out:match("Mem%s*:%s*([%d%.]+)%s*total.-([%d%.]+)%s*used")
  if total and used then
    return tonumber(used), tonumber(total)
  end
  -- BSD states used and unused; the total is their sum.
  local used_m, unused_m = out:match("PhysMem:%s*(%d+)M%s*used.-(%d+)M%s*unused")
  if used_m and unused_m then
    return tonumber(used_m), tonumber(used_m) + tonumber(unused_m)
  end
  return nil
end

--- The three load averages, as text. Both dialects print them the same way.
local function load_of(out)
  local one, five, fifteen =
    out:match("load average[s]?:%s*([%d%.]+)[,%s]+([%d%.]+)[,%s]+([%d%.]+)")
  if not one then
    return nil
  end
  return { one, five, fifteen }
end

--- `15603.1 / 32017.1` MiB as `15.2/31.3 GiB`.
---
--- One unit for the pair, not two: `17.9 GiB / 31.3 GiB` is nineteen columns for
--- thirteen columns of information, and in a 30%-wide side column that difference
--- is the whole reason it did not fit.
local function gib_pair(used, total)
  return string.format("%.1f/%.1f GiB", (used or 0) / 1024, (total or 0) / 1024)
end

--- The process rows: procps and BSD both end with COMMAND, so the last field is
--- the name and `%CPU`/`%MEM` sit at known offsets from it. Parsed loosely on
--- purpose — a row that does not match is skipped rather than drawn wrong.
local function processes(out)
  local rows = {}
  local seen_header = false
  for line in out:gmatch("[^\n]+") do
    if line:match("%s+PID%s") or line:match("^PID%s") then
      seen_header = true
    elseif seen_header then
      local fields = {}
      for field in line:gmatch("%S+") do
        fields[#fields + 1] = field
      end
      -- procps: … %CPU %MEM TIME+ COMMAND. Enough fields, and a numeric %CPU
      -- three from the end, is the shape worth trusting.
      if #fields >= 4 then
        local name = fields[#fields]
        local cpu = tonumber(fields[#fields - 3])
        local mem = tonumber(fields[#fields - 2])
        if name and cpu and mem then
          rows[#rows + 1] = { name = name, cpu = cpu, mem = mem }
        end
      end
    end
    if #rows >= PROCESSES then
      break
    end
  end
  return rows
end

--- A labelled gauge on one line: `CPU  ████░░░░  38%  11.2/15.9 GiB`.
---
--- The bar is sized from what is actually left after the label, the percentage AND
--- the detail — budgeting for the first two only is how `17.5 GiB /` ended up
--- clipped at the pane edge in a 30%-wide column. When there is not room for both,
--- the DETAIL goes: a percentage you cannot read is useless, and the gauge beside
--- it already carries the same number approximately.
local function meter(label, ratio, detail, width)
  local LABEL, PCT, GAP, INDENT, BORDERS = 5, 6, 3, 2, 2
  -- Truncated to what is LEFT rather than dropped when the arithmetic says it
  -- will not fit. Budgeting by subtraction is how `31.3 GiB` became `31.3 Gi` at
  -- the pane edge twice: every fix is one column short of the next terminal
  -- width. Deciding the bar first and handing the detail exactly the remainder
  -- cannot overflow, whatever the width turns out to be.
  local inner = math.max(0, width - BORDERS)
  local fixed = INDENT + LABEL + PCT
  -- The DETAIL is served first and the bar takes what is left, which is the
  -- opposite of the obvious order and the right one: a bar is legible at any
  -- length above a handful of columns, and a number truncated to `17.… GiB` is
  -- legible at none. Sizing the bar first gave it twenty-six columns and the
  -- detail eight, which is how that string reached the screen.
  local shown = nil
  if detail then
    local wanted = GAP + widgets.len(detail)
    if inner - fixed - wanted >= 6 then
      shown = detail
    end
  end
  local bar = math.max(4, math.min(inner - fixed - (shown and (GAP + widgets.len(shown)) or 0), 28))
  local filled = math.floor(ratio * bar + 0.5)
  return {
    type = "text",
    len = 1,
    text = {
      {
        { text = "  " .. widgets.pad(label, 5), style = { fg = theme.secondary } },
        { text = string.rep("█", filled), style = { fg = pressure(ratio) } },
        { text = string.rep("░", bar - filled), style = { fg = theme.muted } },
        {
          text = string.format("  %3d%%", math.floor(ratio * 100 + 0.5)),
          style = { fg = theme.text, bold = true },
        },
        { text = shown and ("   " .. shown) or "", style = { fg = theme.muted } },
      },
    },
  }
end

local function selected_session()
  local id = store.selected
  if not id then
    return nil
  end
  for _, session in ipairs((thurbox and thurbox.sessions) or {}) do
    if session.id == id then
      return session
    end
  end
  return nil
end

--- A single line of explanation in the pane's own frame, for the cases where
--- there is nothing to draw yet. A blank pane is indistinguishable from a broken
--- one, which is the state this exists to avoid.
local function saying(ctx, text)
  return {
    type = "box",
    frame = widgets.panel("System", ctx.focused),
    children = { { type = "text", text = theme.dim("  " .. text) } },
  }
end

return {
  name = "top",
  -- Its own slot, placed by `docs/examples/layout.lua` — copy that too, or this
  -- pane loads and never draws (`plugin list` says `no slot`, which is the fastest
  -- way to spot it). To use it with the SHIPPED arrangement instead, set
  -- `slot = "center"` and `slot_mode = "switch"`: it then shares the centre with
  -- the agent and F7 brings it forward.
  slot = "top",
  order = 85,
  focusable = true,

  -- Declaring it is not being granted it.
  capabilities = { "run" },

  keys = {
    { key = "f7", action = "top.open", desc = "cpu + memory", scope = "global", group = "UI" },
    { key = "r", action = "top.refresh", desc = "read it again now", group = "System" },
  },

  render = function(ctx)
    if not run then
      return saying(ctx, "not trusted yet — settings → Interface → t")
    end
    local session = selected_session()
    if not session then
      return saying(ctx, "no session selected")
    end

    -- Asked every frame, on purpose: the TTL decides whether a process actually
    -- runs, and a fresh answer is a table lookup. Trying to ask "only once" is
    -- how you get a pane that never updates.
    run("sys", COMMAND, { session = session.id, ttl = TTL })

    local answer = (thurbox.runs or {})["sys"]
    if not answer or answer.state == "pending" then
      return saying(ctx, "reading…")
    end
    if answer.state == "failed" then
      return saying(ctx, "could not run top: " .. (answer.error or "unknown"))
    end
    if not answer.ok then
      return saying(ctx, "top exited " .. tostring(answer.status or "?"))
    end

    local out = answer.stdout or ""
    local children = {}
    local cpu = cpu_of(out)
    local used, total = mem_of(out)

    if not cpu and not used then
      -- Both dialects missed. Say so rather than draw empty gauges, and show
      -- what arrived so the reader can see which `top` this is.
      children[#children + 1] = {
        type = "text",
        len = 1,
        text = theme.dim("  top output not recognised:"),
      }
      children[#children + 1] = {
        type = "text",
        text = { { { text = out:sub(1, 400), style = { fg = theme.muted } } } },
      }
      return {
        type = "box",
        frame = widgets.panel("System", ctx.focused),
        children = children,
      }
    end

    -- Where this is being measured. Named first, because the number below means
    -- something different on another machine.
    children[#children + 1] = {
      type = "text",
      len = 1,
      text = {
        {
          { text = "  " .. (session.name or ""), style = { fg = theme.accent, bold = true } },
          {
            text = session.remote_host and ("  " .. session.remote_host) or "  local",
            style = { fg = theme.muted },
          },
        },
      },
    }
    children[#children + 1] = { type = "text", len = 1, text = { { { text = "" } } } }

    if cpu then
      children[#children + 1] = meter("CPU", cpu, nil, ctx.width)
    end
    if used and total and total > 0 then
      children[#children + 1] = meter("MEM", used / total, gib_pair(used, total), ctx.width)
    end

    local load = load_of(out)
    if load then
      children[#children + 1] = {
        type = "text",
        len = 1,
        text = {
          {
            { text = "  " .. widgets.pad("LOAD", 5), style = { fg = theme.secondary } },
            { text = load[1], style = { fg = theme.text, bold = true } },
            {
              text = "   " .. load[2] .. "   " .. load[3],
              style = { fg = theme.muted },
            },
            { text = "     1m  5m  15m", style = { fg = theme.muted } },
          },
        },
      }
    end

    local procs = processes(out)
    if #procs > 0 then
      children[#children + 1] = widgets.divider(ctx.width - 2)
      local rows = {}
      for _, proc in ipairs(procs) do
        rows[#rows + 1] = {
          spans = {
            { text = "  " .. widgets.truncate(proc.name, 22), style = { fg = theme.text } },
            {
              text = string.format("%22s", string.format("%5.1f%% cpu", proc.cpu)),
              style = { fg = pressure(proc.cpu / 100) },
            },
            {
              text = string.format("%12s", string.format("%4.1f%% mem", proc.mem)),
              style = { fg = theme.muted },
            },
          },
        }
      end
      local list = widgets.list({ rows = rows, height = #rows })
      list.fill = 1
      children[#children + 1] = list
    end

    if answer.truncated then
      children[#children + 1] = {
        type = "text",
        len = 1,
        text = theme.dim("  output truncated by the kernel"),
      }
    end
    children[#children + 1] = widgets.hints({ { "r", "read again" } })

    return {
      type = "box",
      frame = widgets.panel("System", ctx.focused),
      children = children,
    }
  end,

  on_action = function(action)
    if action == "top.open" then
      command("focus", { text = "top" })
      return true
    end
    if action == "top.refresh" then
      -- `refresh` is what overrides freshness. Without it this key would do
      -- nothing, because the answer it asks for is the one already on screen.
      local session = selected_session()
      if run and session then
        run("sys", COMMAND, { session = session.id, refresh = true })
      end
      return true
    end
    return false
  end,
}
