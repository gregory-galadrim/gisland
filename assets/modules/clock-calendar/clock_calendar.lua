local platform = require("platform")
local defaults = { locale = nil, timezone = nil, week_start = "monday" }
local weekday_names = { "Di", "Lu", "Ma", "Me", "Je", "Ve", "Sa" }
local function leap(year) return year % 4 == 0 and (year % 100 ~= 0 or year % 400 == 0) end
local function days_in_month(year, month)
  if month == 2 then return leap(year) and 29 or 28 end
  if month == 4 or month == 6 or month == 9 or month == 11 then return 30 end
  return 31
end
local function ordinal(year, month, day)
  local adjusted = month <= 2 and year - 1 or year
  local era = math.floor(adjusted / 400)
  local yoe = adjusted - era * 400
  local shifted = month + (month > 2 and -3 or 9)
  local doy = math.floor((153 * shifted + 2) / 5) + day - 1
  return era * 146097 + yoe * 365 + math.floor(yoe / 4) - math.floor(yoe / 100) + doy - 719468
end
local function civil(value)
  local shifted = value + 719468
  local era = math.floor(shifted / 146097)
  local doe = shifted - era * 146097
  local yoe = math.floor((doe - math.floor(doe / 1460) + math.floor(doe / 36524) - math.floor(doe / 146096)) / 365)
  local year = yoe + era * 400
  local doy = doe - (365 * yoe + math.floor(yoe / 4) - math.floor(yoe / 100))
  local month0 = math.floor((5 * doy + 2) / 153)
  local day = doy - math.floor((153 * month0 + 2) / 5) + 1
  local month = month0 + (month0 < 10 and 3 or -9)
  return year + (month <= 2 and 1 or 0), month, day
end

return function(api, supplied)
  local options = platform.copy_options(defaults, supplied, "clock-calendar")
  if options.week_start ~= "monday" and options.week_start ~= "sunday" then error("week_start must be monday or sunday") end
  local displayed_year, displayed_month, follows, stopped = nil, nil, true, false
  local function now() return api.time.now { timezone = options.timezone, locale = options.locale } end
  local selected
  local function publish(transitions)
    local current = now()
    if follows or displayed_year == nil then displayed_year, displayed_month = current.year, current.month end
    selected = selected or { year = current.year, month = current.month, day = current.day }
    local weekday = (ordinal(displayed_year, displayed_month, 1) + 4) % 7
    local offset = options.week_start == "monday" and (weekday + 6) % 7 or weekday
    local start = ordinal(displayed_year, displayed_month, 1) - offset
    local columns = {}
    for day_index = 0, 6 do
      local children = { api.ui.text { value = weekday_names[(day_index + (options.week_start == "sunday" and 0 or 1)) % 7 + 1], role = "caption" } }
      for week = 0, 5 do
        local year, month, day = civil(start + week * 7 + day_index)
        local today = year == current.year and month == current.month and day == current.day
        local active = year == selected.year and month == selected.month and day == selected.day
        children[#children + 1] = api.ui.button {
          action_id = string.format("select:%04d-%02d-%02d", year, month, day),
          accessible_label = string.format("%d/%d/%d", day, month, year),
          style = active and "day-selected" or today and "day-today" or "day",
          content = api.ui.text { value = tostring(day), role = active and "current" or month == displayed_month and "calendar-day" or "muted" },
        }
      end
      columns[#columns + 1] = api.ui.column { gap = "xsmall", grow = 1, children = children }
    end
    local expanded = api.ui.column { gap = "large", alignment = "stretch", children = {
      api.ui.row { gap = "small", children = {
        api.ui.button { action_id = "previous-month", accessible_label = "Mois précédent", style = "icon", content = api.ui.icon { name = "chevron-left" } },
        api.ui.spacer { flexible = true },
        api.ui.column { gap = "none", children = {
          api.ui.text { value = api.time.format({ year = displayed_year, month = displayed_month, day = 1, source = current }, "%B"):gsub("^%l", string.upper), role = "calendar-month" },
          api.ui.text { value = tostring(displayed_year), role = "muted" },
        } },
        api.ui.spacer { flexible = true },
        api.ui.button { action_id = "next-month", accessible_label = "Mois suivant", style = "icon", content = api.ui.icon { name = "chevron-right" } },
      } },
      api.ui.row { gap = "xsmall", children = columns },
      api.ui.divider {},
      api.ui.row { gap = "small", children = {
        api.ui.text { value = api.time.format({ year = selected.year, month = selected.month, day = selected.day, source = current }, "%A %d %B"):gsub("^%l", string.upper), role = "muted" },
        api.ui.spacer { flexible = true },
        api.ui.button { action_id = "today", accessible_label = "Aujourd’hui", style = "link", content = api.ui.text { value = "Aujourd’hui", role = "accent" } },
      } },
    } }
    local spec = { context_id = "clock", priority = 10, views = {
      compact = api.ui.row { gap = "small", children = {
        api.ui.text { value = api.time.format(current, "%H:%M"), role = "compact-primary" },
        api.ui.spacer { flexible = true }, api.ui.text { value = api.time.format(current, "%a %d %b"), role = "compact-secondary" },
      } }, expanded = expanded }, presentation = { expanded_style = "calendar" } }
    if transitions then spec.transitions = transitions end
    api.publish(spec)
  end
  local function schedule()
    if stopped then return end
    local current = now()
    api.timer.after(60000 - current.second * 1000 - (current.millisecond or 0), function() publish() schedule() end)
  end
  local function shift(delta)
    local current = now()
    if follows or displayed_year == nil then displayed_year, displayed_month = current.year, current.month end
    local index = displayed_year * 12 + displayed_month - 1 + delta
    local year, month = math.floor(index / 12), index % 12 + 1
    if year < 1 or year > 9999 then return false end
    displayed_year, displayed_month, follows = year, month, false
    api.timer.defer(function() publish(delta > 0 and { expanded = "slide-left" } or { expanded = "slide-right" }) end)
    return true
  end
  return { start = function() publish() schedule() end, stop = function() stopped = true end,
    fallback_action = function(action)
      local year, month, day = action:match("^select:(%d%d%d%d)%-(%d%d)%-(%d%d)$")
      year, month, day = tonumber(year), tonumber(month), tonumber(day)
      if not year or year < 1 or month < 1 or month > 12 or day < 1 or day > days_in_month(year, month) then return false end
      selected = { year = year, month = month, day = day }
      publish()
      return true
    end,
    actions = { ["previous-month"] = function() return shift(-1) end, ["next-month"] = function() return shift(1) end,
      today = function() selected = nil local current = now() displayed_year, displayed_month, follows = current.year, current.month, true api.timer.defer(publish) return true end } }
end
