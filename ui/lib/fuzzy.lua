-- Subsequence matching, and the spans that show where it hit.
--
-- A port of `src/fuzzy.rs`, which is what v1 searches with: the query's
-- characters have to appear in order, but not together — `fb` finds `foo-bar`.
-- Plain substring matching would not, and that is the whole difference between a
-- search you type three letters into and one you have to spell for.
--
-- It lives here rather than inside the search pane because two panes need the
-- same answer: search decides WHAT matches, and the pane holding the row decides
-- how to draw it — and they must agree on which characters hit, or the
-- highlight lands on the wrong letters.
--
-- Positions are CHARACTER indices, 1-based, not byte offsets. The Rust version
-- returns byte positions because its caller slices bytes; here the caller slices
-- with `widgets` helpers, which count characters.

local fuzzy = {}

--- The characters of a string, lowercased, as a list.
---
--- `string.lower` only folds ASCII, so a query in another script matches
--- case-sensitively. That is the same limit v1's own picker has in practice and
--- it fails in the safe direction: a miss, never a wrong hit.
local function chars(text)
  local out = {}
  for _, code in utf8.codes(text or "") do
    out[#out + 1] = utf8.char(code):lower()
  end
  return out
end

--- A query compiled once, for matching against many haystacks.
---
--- `fuzzy.match` accepts either a string or one of these; the panes that match
--- one query against every visible row compile it once per render instead of
--- re-splitting the query into characters per field per row.
function fuzzy.compile(query)
  return chars(query)
end

--- Where `query` (a string, or a needle from [`fuzzy.compile`]) matches
--- `haystack`, or nil when it does not.
---
--- An empty query matches everything with no positions — "no filter" rather than
--- "no results", which is what makes an empty search strip show the whole list.
function fuzzy.match(query, haystack)
  local needle = type(query) == "table" and query or chars(query)
  if #needle == 0 then
    return {}
  end

  local positions = {}
  local wanted = 1
  local index = 0
  for _, code in utf8.codes(haystack or "") do
    index = index + 1
    if utf8.char(code):lower() == needle[wanted] then
      positions[#positions + 1] = index
      wanted = wanted + 1
      if wanted > #needle then
        return positions
      end
    end
  end
  return nil
end

--- Does `query` (string or compiled needle) match any of several fields?
--- Returns the field name and its positions, so a caller can say *where* it
--- matched.
---
--- Ordered by the caller: the first field that matches wins, which is why the
--- name is passed first — a row highlights its name when the name matched, and
--- explains itself when something else did.
function fuzzy.first(query, fields)
  local needle = type(query) == "table" and query or chars(query)
  for _, field in ipairs(fields) do
    local positions = fuzzy.match(needle, field.text)
    if positions then
      return field.name, positions, field.text
    end
  end
  return nil
end

--- `text` as spans, with the matched characters styled differently.
---
--- Adjacent matched characters are merged into one span rather than emitted per
--- character: a query that matches a whole word would otherwise produce a span
--- per letter, which is the same picture at several times the cost.
function fuzzy.spans(text, positions, base, hit)
  if not positions or #positions == 0 then
    return { { text = text, style = base } }
  end

  local matched = {}
  for _, position in ipairs(positions) do
    matched[position] = true
  end

  -- Byte offset where character `n` starts. One past the end for `n` just past
  -- the last character, so a run ending at the end slices cleanly.
  local function byte_at(n)
    return utf8.offset(text, n) or (#text + 1)
  end

  local spans = {}
  local total = utf8.len(text) or #text
  local run_start, run_hit = 1, matched[1] == true
  local function flush(stop)
    if stop < run_start then
      return
    end
    spans[#spans + 1] = {
      text = string.sub(text, byte_at(run_start), byte_at(stop + 1) - 1),
      style = run_hit and hit or base,
    }
  end

  for index = 2, total do
    local is_hit = matched[index] == true
    if is_hit ~= run_hit then
      flush(index - 1)
      run_start, run_hit = index, is_hit
    end
  end
  flush(total)
  return spans
end

-- ── The search grammar ──────────────────────────────────────────────────────
--
-- The search strip's query language, parsed once and shared with every pane
-- that lights its own rows against it. The TERMS half mirrors
-- `kernel::search::Query`, which matches the same text against terminal
-- history; this half matches it against session metadata and strips the
-- filters the kernel never sees.
--
--   words        every one must match, in any order
--   "a phrase"   verbatim, as a substring only
--   /regex/      terminal text only — metadata has no regex engine here
--   in:name      only sessions whose name contains `name`
--   repo:name    only sessions whose repository contains `name`
--
-- Case is ignored unless the terms carry a capital letter.

local FILTERS = { ["in"] = true, repo = true }

--- Where a `/regex/` token that opens `text` (at byte `at`) closes: the first
--- unescaped `/` followed by a space or the end, as the kernel reads it.
local function regex_end(text, at)
  local escaped = false
  for i = at + 1, #text do
    local c = text:sub(i, i)
    if c == "\\" and not escaped then
      escaped = true
    else
      if c == "/" and not escaped and i > at + 1 then
        local after = text:sub(i + 1, i + 1)
        if after == "" or after:match("%s") then
          return i
        end
      end
      escaped = false
    end
  end
  return nil
end

--- Parse a query. `terms` is what is left for the kernel once the filters are
--- taken out, spelled as typed so the two parsers read the same thing.
function fuzzy.query(text)
  local q = { words = {}, phrases = {}, regex = false, filters = {}, raw = text or "" }
  local kept = {}
  local at = 1
  text = text or ""
  while at <= #text do
    local c = text:sub(at, at)
    if c:match("%s") then
      at = at + 1
    elseif c == '"' then
      local close = text:find('"', at + 1, true)
      local phrase = text:sub(at + 1, (close or (#text + 1)) - 1)
      if phrase ~= "" then
        q.phrases[#q.phrases + 1] = phrase
        kept[#kept + 1] = '"' .. phrase .. '"'
      end
      at = (close or #text) + 1
    elseif c == "/" and regex_end(text, at) then
      local close = regex_end(text, at)
      q.regex = true
      kept[#kept + 1] = text:sub(at, close)
      at = close + 1
    else
      local stop = (text:find("%s", at) or (#text + 1)) - 1
      local token = text:sub(at, stop)
      local key, value = token:match("^(%a+):(.+)$")
      if key and FILTERS[key] then
        q.filters[key] = value:lower()
      else
        q.words[#q.words + 1] = token
        kept[#kept + 1] = token
      end
      at = stop + 1
    end
  end
  q.terms = table.concat(kept, " ")
  q.case = q.terms:find("%u") ~= nil
  q.empty = #kept == 0
  local fold = q.case and function(s)
    return s
  end or string.lower
  q.fold = fold
  q.needles = {}
  for _, word in ipairs(q.words) do
    q.needles[#q.needles + 1] =
      { text = fold(word), chars = q.case and nil or chars(word), exact = false }
  end
  for _, phrase in ipairs(q.phrases) do
    q.needles[#q.needles + 1] = { text = fold(phrase), exact = true }
  end
  return q
end

--- Whether `session` passes the query's `in:` and `repo:` filters.
function fuzzy.passes(q, session)
  local name = q.filters["in"]
  if name and not (session.name or ""):lower():find(name, 1, true) then
    return false
  end
  local repo = q.filters.repo
  if repo and not (session.repo or ""):lower():find(repo, 1, true) then
    return false
  end
  return true
end

--- Character index (1-based) of byte `byte` in `text`.
local function char_at(text, byte)
  return (utf8.len(text, 1, byte - 1) or (byte - 1)) + 1
end

--- One needle against one string: positions and whether it hit exactly.
local function hit(q, needle, text)
  local folded = q.fold(text)
  local start = folded:find(needle.text, 1, true)
  if start then
    local first = char_at(folded, start)
    local positions = {}
    for i = 0, (utf8.len(needle.text) or #needle.text) - 1 do
      positions[#positions + 1] = first + i
    end
    return positions, true
  end
  if needle.exact then
    return nil
  end
  -- Subsequence, the way names are typed: `fb` for `fix-branch`. Case-folded by
  -- `fuzzy.match` itself, so a case-sensitive query uses the plain letters.
  if q.case then
    local positions, wanted, index = {}, 1, 0
    local want = {}
    for _, code in utf8.codes(needle.text) do
      want[#want + 1] = utf8.char(code)
    end
    for _, code in utf8.codes(text) do
      index = index + 1
      if utf8.char(code) == want[wanted] then
        positions[#positions + 1] = index
        wanted = wanted + 1
        if wanted > #want then
          return positions, false
        end
      end
    end
    return nil
  end
  local positions = fuzzy.match(needle.chars, text)
  return positions, false
end

--- Match a session's fields against a parsed query.
---
--- Every term must hit SOME field, not necessarily the same one, so
--- `claude thurbox` finds the claude session in the thurbox repository. Returns
--- nil for no match; otherwise `{ score, exact, positions, field, text }` where
--- `positions` are the name's lit characters and `field`/`text` name the first
--- other field that matched, for a row to explain itself with.
---
--- A query holding a regex term matches no metadata: the regex is for terminal
--- text, and Lua patterns are not the same language.
function fuzzy.match_fields(q, fields)
  if q.regex then
    return nil
  end
  local result = { score = 0, exact = true, positions = {} }
  local lit = {}
  for _, needle in ipairs(q.needles) do
    local best
    for rank, field in ipairs(fields) do
      local positions, exact = hit(q, needle, field.text or "")
      if positions then
        local score = (exact and 100 or 40) + (rank == 1 and 50 or 0)
        if not best or score > best.score then
          best = { score = score, exact = exact, positions = positions, field = field }
        end
      end
    end
    if not best then
      return nil
    end
    result.score = result.score + best.score
    result.exact = result.exact and best.exact
    if best.field == fields[1] then
      for _, p in ipairs(best.positions) do
        lit[p] = true
      end
    elseif not result.field then
      result.field, result.text = best.field.name, best.field.text
    end
  end
  for p in pairs(lit) do
    result.positions[#result.positions + 1] = p
  end
  table.sort(result.positions)
  return result
end

return fuzzy
