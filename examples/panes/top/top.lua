local theme = require("lib.theme")
local widgets = require("lib.widgets")

local PROCESSES = 6

local TTL = 2

local COMMAND = "top -bn1 2>/dev/null | head -"
  .. tostring(7 + PROCESSES)
  .. " || top -l 1 -n "
  .. tostring(PROCESSES)

local function pressure(ratio)
  if ratio >= 0.85 then
    return theme.bad
  elseif ratio >= 0.6 then
    return theme.warn
  end
  return theme.ok
end

local function number_before(text, unit)
  local found = text:match("([%d%.]+)%s*" .. unit)
  return found and tonumber(found) or nil
end

local function cpu_of(out)
  local idle = number_before(out, "id[,%s]") or number_before(out, "%%%s*idle")
  if not idle then
    return nil
  end
  return math.max(0, math.min((100 - idle) / 100, 1))
end

local function mem_of(out)
  local total, used = out:match("Mem%s*:%s*([%d%.]+)%s*total.-([%d%.]+)%s*used")
  if total and used then
    return tonumber(used), tonumber(total)
  end
  local used_m, unused_m = out:match("PhysMem:%s*(%d+)M%s*used.-(%d+)M%s*unused")
  if used_m and unused_m then
    return tonumber(used_m), tonumber(used_m) + tonumber(unused_m)
  end
  return nil
end

local function load_of(out)
  local one, five, fifteen =
    out:match("load average[s]?:%s*([%d%.]+)[,%s]+([%d%.]+)[,%s]+([%d%.]+)")
  if not one then
    return nil
  end
  return { one, five, fifteen }
end

local function gib_pair(used, total)
  return string.format("%.1f/%.1f GiB", (used or 0) / 1024, (total or 0) / 1024)
end

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

local function meter(label, ratio, detail, width)
  local LABEL, PCT, GAP, INDENT, BORDERS = 5, 6, 3, 2, 2
  local inner = math.max(0, width - BORDERS)
  local fixed = INDENT + LABEL + PCT
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

local function saying(ctx, text)
  return {
    type = "box",
    frame = widgets.panel("System", ctx.focused),
    children = { { type = "text", text = theme.dim("  " .. text) } },
  }
end

return {
  name = "top",
  slot = "top",
  order = 85,
  focusable = true,

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
      local session = selected_session()
      if run and session then
        run("sys", COMMAND, { session = session.id, refresh = true })
      end
      return true
    end
    return false
  end,
}
