-- Scrolling over items of unequal height.
--
-- `widgets.window` answers the common case — every row one line high — by
-- centring the selection. This module answers the other one: a list whose
-- items own different numbers of rows (a session row with a group header glued
-- on top), where ratatui's `ListState` semantics apply instead — scroll the
-- minimum needed to keep the selection fully visible, from wherever the
-- window already was.

local scroll = {}

--- The minimal scroll that keeps `selected` fully visible, given each item's
--- height. Returns the first item (1-based) and the count of items after it
--- that fit — v1's `visible_count_from_heights`.
function scroll.window_variable(heights, offset, selected, max_height)
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

return scroll
