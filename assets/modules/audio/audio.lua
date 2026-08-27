local platform = require("platform")

local defaults = { step_percent = 5, maximum_percent = 150, hud_duration_ms = 1500 }

return function(api, supplied)
  local options = platform.copy_options(defaults, supplied, "audio")
  options.step_percent = platform.integer(options.step_percent, "step_percent", 1, 25)
  options.maximum_percent = platform.integer(options.maximum_percent, "maximum_percent", 100, 200)
  options.hud_duration_ms = platform.integer(options.hud_duration_ms, "hud_duration_ms", 100, 60000)
  local active, generation, cached = nil, 0, nil

  local function run(argv)
    local result = api.process.run(argv, { timeout_ms = 2000 })
    if result.ok then return result.output or "" end
    local detail = platform.trim(result.output or "")
    if detail == "" then detail = "exited with status " .. tostring(result.status) end
    if result.timed_out then detail = "timed out after 2 seconds" end
    error("pactl failed: " .. detail)
  end

  local function state()
    local volume = run({ "pactl", "get-sink-volume", "@DEFAULT_SINK@" })
    local total, count = 0, 0
    for value in volume:gmatch("/%s*(%d+)%%") do total, count = total + tonumber(value), count + 1 end
    if count == 0 then error("pactl returned an invalid sink volume") end
    local lower = math.floor(total / count)
    local fraction = total / count - lower
    if fraction > 0.5 or fraction == 0.5 and lower % 2 == 1 then lower = lower + 1 end
    local mute = run({ "pactl", "get-sink-mute", "@DEFAULT_SINK@" }):match("^%s*Mute:%s*(%a+)")
    if mute ~= "yes" and mute ~= "no" then error("pactl returned an invalid sink mute state") end
    return { volume_percent = lower, muted = mute == "yes" }
  end

  local function publish(id, view, style)
    if active ~= nil and active ~= id then api.dismiss(active) end
    active, generation = id, generation + 1
    local current = generation
    api.publish { context_id = id, priority = 80, expires_in_ms = options.hud_duration_ms,
      views = { compact = view }, presentation = { compact_style = style, reveal = "compact", duration_ms = options.hud_duration_ms } }
    api.timer.after(options.hud_duration_ms, function()
      if active == id and generation == current then
        api.overlay.close()
        active, cached = nil, nil
      end
    end)
  end

  local function meter(before, after)
    local icon = (after.muted or after.volume_percent == 0) and "volume-muted" or
      (after.volume_percent <= options.maximum_percent / 2 and "volume-low" or "volume-high")
    return api.ui.row { gap = "normal", children = {
      api.ui.icon { name = icon, role = "hud-volume-icon", accessible_label = "Volume " .. tostring(after.volume_percent) .. " percent" },
      api.ui.progress { value = platform.clamp(after.volume_percent / options.maximum_percent, 0, 1),
        transition_from = platform.clamp(before.volume_percent / options.maximum_percent, 0, 1), state = "foreground" },
    } }
  end

  local function change(delta)
    local before = cached or state()
    local target = platform.clamp(before.volume_percent + delta, 0, options.maximum_percent)
    if before.muted then run({ "pactl", "set-sink-mute", "@DEFAULT_SINK@", "0" }) end
    if target ~= before.volume_percent then
      run({ "pactl", "set-sink-volume", "@DEFAULT_SINK@", tostring(target) .. "%" })
    end
    local after = { volume_percent = target, muted = false }
    cached = after
    publish("audio-volume", meter(before, after), "hud-meter")
    return true
  end

  return {
    actions = {
      ["volume-up"] = function() return change(options.step_percent) end,
      ["volume-down"] = function() return change(-options.step_percent) end,
      ["toggle-mute"] = function()
        local before = cached or state()
        run({ "pactl", "set-sink-mute", "@DEFAULT_SINK@", before.muted and "0" or "1" })
        local after = { volume_percent = before.volume_percent, muted = not before.muted }
        cached = after
        publish("audio-volume", meter(before, after), "hud-meter")
        return true
      end,
    },
    stop = function() active, cached = nil, nil end,
  }
end
