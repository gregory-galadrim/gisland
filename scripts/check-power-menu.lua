package.path = "assets/?.lua;assets/?/init.lua;" .. package.path

local published, commands, responses, closes = nil, {}, {}, 0
local function node(kind)
  return function(value) value.type = kind return value end
end
local api = {
  ui = {},
  process = { run = function(argv)
    commands[#commands + 1] = table.concat(argv, "\0")
    return table.remove(responses, 1) or { ok = true, status = 0, output = "" }
  end },
  publish = function(spec) published = spec end,
  dismiss = function() end,
  overlay = { close = function() closes = closes + 1 end },
  log = function() end,
}
for _, kind in ipairs { "text", "row", "column", "spacer", "button", "action_region", "icon" } do
  api.ui[kind] = node(kind)
end

local function action_ids(value, result)
  result = result or {}
  if type(value) ~= "table" then return result end
  if value.action_id then result[value.action_id] = true end
  for _, child in pairs(value) do action_ids(child, result) end
  return result
end
local function expect_commands(...)
  local expected = { ... }
  assert(#commands == #expected, "unexpected command count")
  for index, command in ipairs(expected) do assert(commands[index] == command, "unexpected command") end
end
local function activate(instance)
  commands, responses = {}, {}
  instance.activate()
  return action_ids(published.views.expanded)
end

local instance = assert(dofile("assets/modules/power-menu/power_menu.lua"))(api, {})
local actions = activate(instance)
for _, id in ipairs { "lock", "sleep", "reboot", "power-off" } do assert(actions[id], "missing " .. id) end

instance.actions["power-off"]()
expect_commands()
assert(action_ids(published.views.expanded)["confirm-power-off"])
instance.actions.cancel()
assert(action_ids(published.views.expanded)["power-off"])

instance.actions["power-off"]()
instance.actions["confirm-power-off"]()
expect_commands("systemctl\0poweroff")

activate(instance)
instance.actions.reboot()
expect_commands()
instance.actions["confirm-reboot"]()
expect_commands("systemctl\0reboot")

activate(instance)
instance.input("down")
instance.input("down")
instance.input("enter")
expect_commands()
assert(action_ids(published.views.expanded)["confirm-reboot"])
instance.input("right")
instance.input("enter")
expect_commands("systemctl\0reboot")

activate(instance)
instance.input("down")
instance.input("down")
instance.input("enter")
instance.input("escape")
assert(action_ids(published.views.expanded).reboot, "escape did not cancel confirmation")

activate(instance)
instance.actions.sleep()
expect_commands("loginctl\0lock-sessions", "systemctl\0suspend")

local closed_before_failure = closes
activate(instance)
responses = { { ok = false, status = 1, output = "denied" } }
instance.actions.sleep()
expect_commands("loginctl\0lock-sessions")
assert(closes == closed_before_failure, "failure closed the overlay")
assert(action_ids(published.views.expanded).back, "failure view is not visible")

activate(instance)
local closed_before_escape = closes
instance.input("escape")
assert(closes == closed_before_escape + 1, "escape did not close the power menu")

print("power-menu check passed")
