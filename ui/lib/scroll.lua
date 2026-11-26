local scroll = {}

function scroll.window_variable(heights, offset, selected, max_height)
  local count = #heights
  if count == 0 or max_height <= 0 then
    return 1, 0
  end

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
