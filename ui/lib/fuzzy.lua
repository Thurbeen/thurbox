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

--- Where `query` matches `haystack`, or nil when it does not.
---
--- An empty query matches everything with no positions — "no filter" rather than
--- "no results", which is what makes an empty search strip show the whole list.
function fuzzy.match(query, haystack)
  local needle = chars(query)
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

--- Does `query` match any of several fields? Returns the field name and its
--- positions, so a caller can say *where* it matched.
---
--- Ordered by the caller: the first field that matches wins, which is why the
--- name is passed first — a row highlights its name when the name matched, and
--- explains itself when something else did.
function fuzzy.first(query, fields)
  for _, field in ipairs(fields) do
    local positions = fuzzy.match(query, field.text)
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

return fuzzy
