local function integer(value, name)
  if type(value) ~= "number" or value ~= math.floor(value) then error(name .. " must be an integer") end
  return value
end

local defaults = {
  warning_percent = 20, persistent_percent = 10, critical_percent = 5,
  yellow_percent = 30, red_percent = 15, preview_duration_ms = 3000,
}

return function(api, supplied)
  supplied = supplied or {}
  local options = {}
  for name, default in pairs(defaults) do
    options[name] = supplied[name] == nil and default or integer(supplied[name], name)
  end
  for name in pairs(supplied) do if defaults[name] == nil then error("unknown battery option: " .. name) end end
  if options.critical_percent <= 0 or options.critical_percent >= options.persistent_percent or
      options.persistent_percent >= options.warning_percent or options.warning_percent > 100 then
    error("battery thresholds must satisfy 0 < critical < persistent < warning <= 100")
  end
  if options.red_percent <= 0 or options.red_percent >= options.yellow_percent or options.yellow_percent > 100 then
    error("battery colours must satisfy 0 < red < yellow <= 100")
  end
  if options.preview_duration_ms < 0 or options.preview_duration_ms > 60000 then
    error("preview_duration_ms must be between 0 and 60000")
  end

  local stopped, root_proxy, device_proxy, active_alert = false, nil, nil, nil
  local last_snapshot, last_publish
  local saved = api.state.load("battery-cycle", {}) or {}
  local emitted = saved.emitted or {}
  local previous_on_battery = saved.previous_on_battery

  local function save_state()
    api.state.save("battery-cycle", {
      version = 1, emitted = emitted, previous_on_battery = previous_on_battery,
    })
  end

  local function round(value)
    local lower = math.floor(value)
    local fraction = value - lower
    if fraction > 0.5 or fraction == 0.5 and lower % 2 == 1 then return lower + 1 end
    return lower
  end

  local function duration(seconds)
    if not seconds or seconds <= 0 then return nil end
    local minutes = math.floor((seconds + 30) / 60)
    local hours = math.floor(minutes / 60)
    if hours > 0 then return string.format("%d h %02d", hours, minutes % 60) end
    return tostring(minutes) .. " min"
  end

  local function snapshot(reading)
    local percentage = math.max(0, math.min(reading.percentage, 100))
    local estimate = duration(reading.on_battery and reading.time_to_empty or reading.time_to_full)
    local state_text = reading.state == "charging" and "Charge" or
      reading.state == "fully-charged" and "Chargée" or
      reading.on_battery and "Sur batterie" or "Secteur"
    local health = "—"
    if reading.energy_full > 0 and reading.energy_full_design > 0 then
      health = tostring(round(reading.energy_full / reading.energy_full_design * 100)) .. " %"
    end
    local power = reading.energy_rate > 0 and string.format("%.1f W", reading.energy_rate):gsub("%.", ",") or "—"
    return {
      level = percentage / 100, percent_text = tostring(round(percentage)) .. " %",
      semantic_state = percentage > options.yellow_percent and "success" or
        percentage > options.red_percent and "warning" or "error",
      estimate_compact = estimate or "Calcul…", state_text = state_text,
      estimate_detail = estimate and (estimate .. (reading.on_battery and " restantes" or " avant charge complète")) or "—",
      health_text = health, power_text = power,
    }
  end

  local ui = api.ui
  local function row(label, value)
    return ui.row { gap = "small", children = {
      ui.text { value = label, role = "muted" }, ui.spacer { flexible = true }, ui.text { value = value, role = "body" },
    } }
  end
  local function compact_view(value)
    return ui.row { gap = "small", children = {
      ui.progress { shape = "ring", value = value.level, state = value.semantic_state },
      ui.text { value = value.percent_text, role = "battery-compact" }, ui.spacer { flexible = true },
      ui.text { value = value.estimate_compact, role = "caption" },
    } }
  end
  local function expanded_view(value, dismissible)
    if dismissible then
      return ui.row { gap = "normal", children = {
        ui.icon { name = "warning", state = value.semantic_state, size = 24 },
        ui.column { alignment = "start", grow = 1, gap = "xsmall", children = {
          ui.text { value = "Batterie faible · " .. value.percent_text, role = "body" },
          ui.text { value = "Branchez votre ordinateur.", role = "muted" },
        } },
        ui.button { action_id = "dismiss-alert", accessible_label = "Fermer l’alerte", style = "icon", content = ui.icon { name = "close" } },
      } }
    end
    return ui.column { gap = "normal", alignment = "stretch", children = {
      ui.row { gap = "large", children = {
        ui.progress { shape = "ring", size = 64, value = value.level, state = value.semantic_state },
        ui.column { alignment = "start", gap = "large", children = {
          ui.text { value = value.percent_text, role = "battery-value" },
          ui.text { value = value.state_text, role = "muted" },
        } },
      } },
      ui.spacer { size = 0 },
      ui.divider {}, row("Autonomie", value.estimate_compact), row("Santé", value.health_text), row("Puissance", value.power_text),
    } }
  end

  local function publish_alert(kind, value)
    if active_alert then api.dismiss(active_alert) end
    local persistent = kind == "persistent" or kind == "critical"
    local context_id = "battery-" .. kind
    local spec = {
      context_id = context_id,
      priority = ({ plugged = 35, unplugged = 35, warning = 40, persistent = 60, critical = 90 })[kind],
      views = { compact = compact_view(value), expanded = expanded_view(value, persistent) },
      presentation = { compact_style = "battery", expanded_style = persistent and "battery-alert" or "battery" },
    }
    if persistent then
      spec.presentation.reveal = "expanded"
      active_alert = context_id
    else
      spec.expires_in_ms = options.preview_duration_ms
      if options.preview_duration_ms > 0 then spec.presentation.reveal, spec.presentation.duration_ms = "expanded", options.preview_duration_ms end
      active_alert = nil
    end
    api.publish(spec)
  end

  local function observe(reading)
    local alerts = {}
    if previous_on_battery ~= nil and reading.on_battery ~= previous_on_battery then
      if reading.on_battery then emitted, alerts = {}, { "unplugged" } else alerts = { "plugged" } end
    end
    previous_on_battery = reading.on_battery
    if not reading.on_battery then return alerts end
    local crossed
    local thresholds = {
      { options.warning_percent, "warning" }, { options.persistent_percent, "persistent" },
      { options.critical_percent, "critical" },
    }
    for _, candidate in ipairs(thresholds) do
      if reading.percentage <= candidate[1] and not emitted[candidate[1]] then
        emitted[candidate[1]] = true
        crossed = candidate[2]
      end
    end
    if crossed then alerts[#alerts + 1] = crossed end
    return alerts
  end

  local function update(reading)
    if stopped or not reading.present or reading.percentage ~= reading.percentage then return end
    local value = snapshot(reading)
    local now = os.clock()
    local immediate = not last_snapshot or value.percent_text ~= last_snapshot.percent_text or
      value.semantic_state ~= last_snapshot.semantic_state or value.state_text ~= last_snapshot.state_text
    if immediate or not last_publish or now - last_publish >= 30 then
      api.publish { context_id = "battery", priority = 10,
        presentation = { compact_style = "battery", expanded_style = "battery" }, views = { compact = compact_view(value), expanded = expanded_view(value, false) } }
      last_snapshot, last_publish = value, now
    end
    local alerts = observe(reading)
    local ok, message = pcall(save_state)
    if not ok then api.log("error", "gisland-battery: cannot save battery state: " .. tostring(message)) end
    for _, alert in ipairs(alerts) do publish_alert(alert, value) end
  end

  local state_names = { [1] = "charging", [2] = "discharging", [3] = "empty", [4] = "fully-charged",
    [5] = "pending-charge", [6] = "pending-discharge" }
  local function property(proxy, name, default)
    local value = proxy.get and proxy:get(name) or proxy.property and proxy:property(name)
    if value == nil then return default end
    return type(value) == "table" and value.value or value
  end
  local function emit_reading()
    if stopped or not root_proxy or not device_proxy then return end
    local state = state_names[math.floor(tonumber(property(device_proxy, "State", 0)))] or "unknown"
    update {
      percentage = tonumber(property(device_proxy, "Percentage", 0 / 0)),
      on_battery = not not property(root_proxy, "OnBattery", state == "discharging"), state = state,
      time_to_empty = tonumber(property(device_proxy, "TimeToEmpty", 0)), time_to_full = tonumber(property(device_proxy, "TimeToFull", 0)),
      energy_rate = tonumber(property(device_proxy, "EnergyRate", 0)), energy_full = tonumber(property(device_proxy, "EnergyFull", 0)),
      energy_full_design = tonumber(property(device_proxy, "EnergyFullDesign", 0)), present = not not property(device_proxy, "IsPresent", false),
    }
  end
  local function watch(proxy)
    if proxy.watch then return proxy:watch(emit_reading) end
    if proxy.on_change then return proxy:on_change(emit_reading) end
  end
  local function poll()
    if stopped then return end
    local ok, message = pcall(emit_reading)
    if not ok then api.log("error", "gisland-battery: UPower read failed: " .. tostring(message)) end
    api.timer.after(30000, poll)
  end

  local function start()
    root_proxy = api.dbus.proxy { bus = "system", name = "org.freedesktop.UPower", path = "/org/freedesktop/UPower", interface = "org.freedesktop.UPower" }
    device_proxy = api.dbus.proxy { bus = "system", name = "org.freedesktop.UPower", path = "/org/freedesktop/UPower/devices/DisplayDevice", interface = "org.freedesktop.UPower.Device" }
    watch(root_proxy); watch(device_proxy); poll()
  end

  return {
    start = function()
      local ok, message = pcall(start)
      if not ok then api.log("error", "gisland-battery: UPower unavailable: " .. tostring(message)) end
    end,
    actions = { ["dismiss-alert"] = function()
      if not active_alert then return false end
      api.dismiss(active_alert); active_alert = nil; return true
    end },
    fallback_action = function() return false end,
    stop = function()
      stopped = true
      if root_proxy and root_proxy.close then root_proxy:close() end
      if device_proxy and device_proxy.close then device_proxy:close() end
      root_proxy, device_proxy = nil, nil
    end,
  }
end
