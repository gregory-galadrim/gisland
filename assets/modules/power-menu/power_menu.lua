local platform = require("platform")

return function(api, supplied)
  if supplied and next(supplied) ~= nil then error("power-menu has no options") end
  local choices = {
    { id = "lock", label = "Lock", icon = "lock" },
    { id = "reboot", label = "Reboot", icon = "reboot" },
    { id = "power-off", label = "Power off", icon = "power-off" },
  }
  local selected, confirmation, failure = 1, nil, nil
  local publish

  local function reset()
    selected, confirmation, failure = 1, nil, nil
  end

  local function close()
    reset()
    api.overlay.close()
    api.dismiss("power-menu")
    return true
  end

  local function run(label, argv, timeout_ms)
    local called, result = pcall(api.process.run, argv, { timeout_ms = timeout_ms or 5000 })
    if called and result and result.ok then return true end
    local detail = called and result and platform.trim(result.output or "") or tostring(result)
    if called and result and result.timed_out then detail = "timed out" end
    if detail == "" then detail = "exited with status " .. tostring(result.status) end
    api.log("error", label .. " failed: " .. detail:sub(1, 512))
    selected, confirmation, failure = 1, nil, "Action failed. Try again."
    publish()
    return false
  end

  local function choice(id, label, icon, active)
    return api.ui.button { action_id = id, accessible_label = label, grow = 1,
      style = active and "session-selected" or "session",
      content = api.ui.column { gap = "normal", children = {
        api.ui.icon { name = icon, size = 24 },
        api.ui.text { value = label, role = "button" },
      } } }
  end

  local function main_view()
    local children = {}
    for index, item in ipairs(choices) do
      children[#children + 1] = choice(item.id, item.label, item.icon, index == selected)
    end
    return api.ui.row { gap = "small", children = children }
  end

  local function confirmation_view(kind)
    local label = kind == "reboot" and "Reboot" or "Power off"
    return api.ui.column { gap = "normal", children = {
      api.ui.text { value = label .. " the computer?", role = "title" },
      api.ui.text { value = "Any unsaved work will be lost.", role = "muted" },
      api.ui.row { gap = "normal", children = {
        api.ui.button { action_id = "cancel", accessible_label = "Cancel", style = "quiet", focused = selected == 1,
          content = api.ui.text { value = "Cancel", role = "button" } },
        api.ui.button { action_id = "confirm-" .. kind, accessible_label = label, style = "danger", focused = selected == 2,
          content = api.ui.text { value = label, role = "button" } },
      } },
    } }
  end

  local function failure_view()
    return api.ui.column { alignment = "start", gap = "small", children = {
      api.ui.text { value = failure, role = "body" },
      choice("back", "Back", "chevron-left", true),
    } }
  end

  publish = function()
    local expanded = failure and failure_view() or confirmation and confirmation_view(confirmation) or main_view()
    api.publish {
      context_id = "power-menu", priority = 1000,
      views = {
        compact = api.ui.text { value = "Session", role = "compact-primary" },
        expanded = expanded,
      },
      presentation = { expanded_style = (confirmation or failure) and "confirmation" or "power-menu" },
    }
  end

  local function lock()
    if run("Lock", { "loginctl", "lock-sessions" }) then close() end
    return true
  end

  local function sleep()
    if run("Lock", { "loginctl", "lock-sessions" }) and
        run("Sleep", { "systemctl", "suspend" }, 60000) then close() end
    return true
  end

  local function confirm(kind)
    selected, confirmation, failure = 1, kind, nil
    publish()
    return true
  end

  local function cancel()
    reset()
    publish()
    return true
  end

  local function confirmed(kind)
    if confirmation ~= kind then return false end
    local label = kind == "reboot" and "Reboot" or "Power off"
    if run(label, { "systemctl", kind == "reboot" and "reboot" or "poweroff" }) then close() end
    return true
  end

  local function choose()
    if failure then return cancel() end
    if confirmation then return selected == 1 and cancel() or confirmed(confirmation) end
    local action = choices[selected].id
    if action == "lock" then return lock() end
    if action == "sleep" then return sleep() end
    return confirm(action)
  end

  local function input(kind)
    if kind == "escape" then return (confirmation or failure) and cancel() or close() end
    if kind == "enter" then return choose() end
    local maximum = confirmation and 2 or failure and 1 or #choices
    local delta = (kind == "up" or kind == "left") and -1 or
      (kind == "down" or kind == "right") and 1 or 0
    local next_selected = platform.clamp(selected + delta, 1, maximum)
    if delta == 0 or next_selected == selected then return false end
    selected = next_selected
    publish()
    return true
  end

  return {
    activate = function() reset() publish() end,
    input = input,
    actions = {
      close = close,
      lock = lock,
      sleep = sleep,
      reboot = function() return confirm("reboot") end,
      ["power-off"] = function() return confirm("power-off") end,
      cancel = cancel,
      back = cancel,
      ["confirm-reboot"] = function() return confirmed("reboot") end,
      ["confirm-power-off"] = function() return confirmed("power-off") end,
    },
    visibility = function(value)
      if value == "hidden" then reset() api.dismiss("power-menu") end
    end,
  }
end
