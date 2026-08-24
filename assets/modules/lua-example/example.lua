return function(api, supplied)
  supplied = supplied or {}
  local format = supplied.format or "%H:%M:%S"
  if type(format) ~= "string" then error("format must be a string") end
  local stopped = false
  local function publish()
    if stopped then return end
    local now = api.time.now()
    api.publish { context_id = "lua-example", priority = 1,
      views = {
        compact = api.ui.text { value = api.time.format(now, format), role = "compact-primary" },
        expanded = api.ui.column { gap = "small", children = {
          api.ui.text { value = api.time.format(now, format), role = "title" },
          api.ui.text { value = api.time.format(now, "%Y-%m-%d"), role = "body" },
        } },
      } }
  end
  local function schedule()
    if not stopped then api.timer.after(1000, function() publish() schedule() end) end
  end
  return { start = function() publish() schedule() end, stop = function() stopped = true end }
end
