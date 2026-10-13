package.path = "assets/?.lua;assets/?/init.lua;" .. package.path
local published
local api = { ui = {}, timer = { after = function() end, defer = function(f) f() end },
  publish = function(spec) published = spec end, dismiss = function() end,
  overlay = { close = function() end },
  time = { now = function() return { year=2026, month=10, day=13, second=0 } end,
    format = function(_, pattern) return ({ ["%H:%M"]="09:41", ["%a %d %b"]="Tue 13 Oct", ["%B"]="October", ["%A %d %B"]="Tuesday 13 October" })[pattern] or "13 October" end },
  process = { run = function(argv)
    return { ok=true, output=argv[2] == "get-sink-volume" and " / 65%" or "Mute: no" }
  end },
}
for _, kind in ipairs { "text", "row", "column", "spacer", "button", "action_region", "icon", "progress", "divider" } do
  api.ui[kind] = function(value) value.type = kind return value end
end
local function find(value, predicate)
  if type(value) ~= "table" then return end
  if predicate(value) then return value end
  for _, child in pairs(value) do local result = find(child, predicate) if result then return result end end
end
local clock = require("modules.clock-calendar.clock_calendar")(api, {})
clock.start()
assert(find(published, function(n) return n.action_id == "select:2026-10-13" end), "calendar dates must be selectable")
assert(clock.fallback_action("select:2026-10-14"))
assert(find(published, function(n) return n.action_id == "select:2026-10-14" and n.style == "day-selected" end), "selected day must be highlighted")
assert(not clock.fallback_action("select:2026-02-31"), "invalid dates must be rejected")
clock.actions["next-month"]()
assert(find(published, function(n) return n.action_id == "select:2026-11-01" end), "month navigation must advance")
clock.actions.today()
assert(find(published, function(n) return n.action_id == "select:2026-10-13" and n.style == "day-selected" end))
local audio = require("modules.audio.audio")(api, {})
audio.actions["volume-up"]()
assert(find(published, function(n) return n.value == "70 %" end), "volume must display a numeric value")
audio.actions["toggle-mute"]()
assert(published.presentation.compact_style == "hud-symbol", "mute must use the small symbol capsule")
for _ = 1, 14 do audio.actions["volume-down"]() end
assert(published.presentation.compact_style == "hud-symbol", "zero volume must use the mute capsule")
print("design interaction checks passed")
