local BUS_NAME, OBJECT_PATH, INTERFACE = "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications"
local UINT32_MAX, MAX_LIVE = 4294967295, 128
local MAX_HISTORY = 1000
local defaults = { reveal_duration_ms = 1000, history_limit = 100, history_visible_limit = 5 }
local capabilities = { "actions", "body", "body-markup", "body-hyperlinks", "body-images", "icon-static", "persistence" }

return function(api, supplied)
  supplied = supplied or {}
  local options = {}
  for name, default in pairs(defaults) do
    local value = supplied[name] == nil and default or supplied[name]
    if type(value) ~= "number" or value ~= math.floor(value) then error(name .. " must be an integer") end
    options[name] = value
  end
  for name in pairs(supplied) do if defaults[name] == nil then error("unknown notification option: " .. name) end end
  if options.reveal_duration_ms < 0 or options.reveal_duration_ms > 60000 then error("reveal_duration_ms is out of range") end
  if options.history_limit < 1 or options.history_limit > MAX_HISTORY then error("history_limit is out of range") end
  if options.history_visible_limit < 1 or options.history_visible_limit > 5 or options.history_visible_limit > options.history_limit then
    error("history_visible_limit is out of range")
  end

  local ui = api.ui
  local stopped, service, next_id = false, nil, 1
  local live, order, routes, timers = {}, {}, {}, {}
  local history, history_sequences, next_sequence = {}, {}, 1
  local visible_count, hidden, session, visibility = 0, {}, 0, "hidden"
  local history_was_expanded, open_timer, inactivity_timer = false, nil, nil

  local function state_get(name) return api.state.load(name, {}) end; local state_set = api.state.save
  local stored = state_get("notifications-history") or {}
  if stored.records and type(stored.records) == "table" then history = stored.records end
  next_sequence = tonumber(stored.next_sequence) or 1
  for _, record in ipairs(history) do history_sequences[record.notification_id] = record.sequence end

  local function log_error(message) api.log("error", "gisland-notifications: " .. tostring(message)) end
  local function save_history()
    local ok, message = pcall(state_set, "notifications-history", { version = 1, next_sequence = next_sequence, records = history })
    if not ok then log_error("could not save notification history: " .. tostring(message)) end
  end
  local function bounded(value, maximum)
    value = tostring(value or "")
    if #value <= maximum then return value end
    local result = value:sub(1, maximum)
    while result:sub(-1):byte() and result:sub(-1):byte() >= 128 and result:sub(-1):byte() < 192 do result = result:sub(1, -2) end
    return result
  end
  local function required(value, maximum, name)
    if type(value) ~= "string" then error(name .. " must be a string") end
    if #value > maximum then error(name .. " exceeds the byte limit") end
    return value
  end

  local function array(values)
    local result = api.array and api.array() or {}
    for index, value in ipairs(values) do result[index] = value end
    return result
  end
  local function plain(value)
    local items = {}
    value = bounded(value, 16384)
    while value ~= "" and #items < 128 do
      local span = value:sub(1, 4096)
      items[#items + 1] = ui.text { value = span, role = "body" }
      value = value:sub(#span + 1)
    end
    return { items = items, links = {}, images = {} }
  end
  local function attr(tag, name)
    return tag:match(name .. "%s*=%s*[\"']([^\"']*)[\"']") or tag:match(name .. "%s*=%s*([^%s>]+)") or ""
  end
  local function parse_body(value)
    local lowered = value:lower()
    if lowered:find("<!doctype", 1, true) or lowered:find("<!entity", 1, true) then return plain(value) end
    local result, links, images = {}, {}, {}
    local emphasis, link_id, link_count, image_count = {}, nil, 0, 0
    local function add_text(text)
      if text == "" or #result >= 128 then return end
      local item = { type = link_id and "link" or "text", value = bounded(text, 4096) }
      if link_id then item.action_id, item.accessible_label = link_id, item.value end
      if #emphasis > 0 then item.emphasis = array(emphasis) end
      result[#result + 1] = item
    end
    local position = 1
    while position <= #value do
      local start, finish, tag = value:find("<[^>]*>", position)
      if not start then add_text(value:sub(position)); break end
      add_text(value:sub(position, start - 1))
      local name = tag:match("^<%s*/?%s*([%w]+)")
      local closing = tag:match("^<%s*/") ~= nil
      name = name and name:lower()
      if name == "b" or name == "i" or name == "u" then
        local named = ({ b = "bold", i = "italic", u = "underline" })[name]
        if closing then for index = #emphasis, 1, -1 do if emphasis[index] == named then table.remove(emphasis, index); break end end
        else emphasis[#emphasis + 1] = named end
      elseif name == "a" then
        if closing then link_id = nil else
          local href = bounded(attr(tag, "href"), 4096)
          if href ~= "" and link_count < 32 then link_id = "link-" .. tostring(link_count); link_count = link_count + 1; links[link_id] = href end
        end
      elseif name == "img" and not closing then
        local source, label = bounded(attr(tag, "src"), 4096), bounded(attr(tag, "alt"), 4096)
        if source ~= "" and #result < 128 then
          local id = "inline-" .. tostring(image_count); image_count = image_count + 1
          images[id] = { source, label }
          result[#result + 1] = { type = "inline_image", resource_id = id, role = "notification-inline-image", accessible_label = label }
        end
      end
      position = finish + 1
    end
    return { items = result, links = links, images = images }
  end

  local function load_image(value)
    if type(value) == "string" and value ~= "" and api.image.load then
      local ok, image = pcall(api.image.load, value)
      if ok then return image end
    end
    if type(value) == "table" and api.image.from_raw then
      local ok, image = pcall(api.image.from_raw, value)
      if ok then return image end
    end
  end
  local function resource(id, image)
    if api.image.resource then return api.image.resource(id, image) end
    return { id = id, image = image }
  end
  local function resolve_image(hints, icon)
    for _, key in ipairs({ "image-data", "image_data", "icon_data" }) do
      if hints[key] then local image = load_image(hints[key]); if image then return image end end
    end
    for _, key in ipairs({ "image-path", "image_path", "desktop-entry" }) do
      if hints[key] then local image = load_image(hints[key]); if image then return image end end
    end
    return load_image(icon)
  end

  local function text(value, role) return ui.text { value = value, role = role } end
  local function notification_view(notification, app_image)
    local parsed = parse_body(notification.body)
    local prefix = "notification-" .. tostring(notification.id) .. ":"
    local routing = { [prefix .. "close"] = { "close" } }
    local resources, app_resource = {}, nil
    if app_image then app_resource = resource("app-image", app_image); resources[#resources + 1] = app_resource end
    local header = {}
    if app_resource then header[#header + 1] = ui.image { resource_id = "app-image", role = "notification-header-icon", accessible_label = notification.app_name ~= "" and notification.app_name or "Application" } end
    if notification.app_name ~= "" then header[#header + 1] = text(notification.app_name:upper(), "caption") end
    if notification.summary ~= "" then header[#header + 1] = text(notification.summary, "body") end
    local top = { ui.row { gap = "small", children = array(header) } }
    local body = {}
    for _, item in ipairs(parsed.items) do
      if item.type == "inline_image" then
        local source = parsed.images[item.resource_id]
        local image = source and load_image(source[1])
        if image then resources[#resources + 1] = resource(item.resource_id, image); body[#body + 1] = item end
      else
        if item.type == "link" then item.action_id = prefix .. item.action_id; routing[item.action_id] = { "uri", parsed.links[item.action_id:sub(#prefix + 1)] } end
        body[#body + 1] = item
      end
    end
    if #body > 0 then top[#top + 1] = ui.rich_text { role = "notification-body", content = array(body) } end
    local buttons, action_index, default = {}, 0, nil
    for index = 1, #notification.actions, 2 do
      local key, label = notification.actions[index], notification.actions[index + 1]
      if key == "default" then default = prefix .. "default"; routing[default] = { "dbus", key }
      else
        local id = prefix .. "action-" .. tostring(action_index); action_index = action_index + 1
        routing[id] = { "dbus", key }; buttons[#buttons + 1] = ui.button { action_id = id, accessible_label = label, content = text(label, "button") }
      end
    end
    if #buttons > 0 then top[#top + 1] = ui.row { gap = "small", children = array(buttons) } end
    local expanded = ui.column { alignment = "start", gap = "small", children = array(top) }
    if default then expanded = ui.action_region { action_id = default, accessible_label = "Open notification", content = expanded } end
    local publication = { context_id = "notification-" .. tostring(notification.id), priority = notification.priority, views = { expanded = expanded } }
    if visible_count == 0 and options.reveal_duration_ms > 0 then publication.presentation = { reveal = "expanded", duration_ms = options.reveal_duration_ms } end
    if #resources > 0 then publication.resources = array(resources) end
    return publication, routing
  end

  local function plain_body(value)
    local parsed, parts = parse_body(value), {}
    for _, item in ipairs(parsed.items) do parts[#parts + 1] = item.value or (parsed.images[item.resource_id] and parsed.images[item.resource_id][2]) or "" end
    return bounded(table.concat(parts), 4096)
  end
  local function add_history(notification)
    local sequence = history_sequences[notification.id]
    for index, record in ipairs(history) do if sequence and record.sequence == sequence then table.remove(history, index); break end end
    if not sequence then sequence, next_sequence = next_sequence, next_sequence + 1 end
    table.insert(history, 1, { sequence = sequence, notification_id = notification.id, app_name = bounded(notification.app_name, 4096), summary = bounded(notification.summary, 4096), body = plain_body(notification.body), received_at = os.time() })
    while #history > options.history_limit do table.remove(history) end
    history_sequences[notification.id] = sequence; save_history()
  end
  local function remove_history(id)
    local sequence = history_sequences[id]; history_sequences[id] = nil
    if not sequence then return end
    for index, record in ipairs(history) do if record.sequence == sequence then table.remove(history, index); save_history(); return end end
  end
  local function cancel(timer)
    if timer and api.timer.cancel then api.timer.cancel(timer) end
  end
  local function close_notification(id, reason)
    local notification = live[id]; if not notification then return false end
    live[id], routes[id] = nil, nil; cancel(timers[id]); timers[id] = nil
    for index, candidate in ipairs(order) do if candidate == id then table.remove(order, index); break end end
    api.dismiss("notification-" .. tostring(id))
    if service and service.signal then service:signal("NotificationClosed", id, reason) end
    return true
  end
  local function allocate_id()
    local candidate = next_id
    while live[candidate] do candidate = candidate == UINT32_MAX and 1 or candidate + 1 end
    next_id = candidate == UINT32_MAX and 1 or candidate + 1
    return candidate
  end
  local function notify(...)
    local values = { ... }; if #values == 1 and type(values[1]) == "table" then values = values[1] end
    local app_name, replaces_id, icon, summary, body = required(values[1], 1024, "app_name"), tonumber(values[2]) or 0, required(values[3], 4096, "app_icon"), required(values[4], 4096, "summary"), required(values[5], 65536, "body")
    local actions, hints, expire = values[6] or {}, values[7] or {}, tonumber(values[8]) or -1
    if #actions % 2 ~= 0 then error("actions must contain key and label pairs") end
    local urgency = tonumber(hints.urgency) or 1; if urgency ~= 0 and urgency ~= 2 then urgency = 1 end
    local timeout = expire > 0 and expire or (expire == 0 or urgency == 2) and nil or (urgency == 0 and 5000 or 8000)
    local id = replaces_id ~= 0 and live[replaces_id] and replaces_id or allocate_id()
    local notification = { id = id, app_name = app_name, summary = summary, body = body, actions = actions, priority = ({ [0] = 10, [1] = 20, [2] = 30 })[urgency], resident = hints.resident == true, transient = hints.transient == true }
    local publication, routing = notification_view(notification, resolve_image(hints, icon))
    if not live[id] and #order >= MAX_LIVE then close_notification(order[1], 4) end
    api.publish(publication); cancel(timers[id]); live[id], routes[id] = notification, routing
    if not replaces_id or not live[replaces_id] then order[#order + 1] = id end
    if notification.transient then remove_history(id) else add_history(notification) end
    if visible_count > 0 then api.publish(history_publication()) end
    if timeout then timers[id] = api.timer.after(timeout, function() timers[id] = nil; close_notification(id, 1) end) end
    return id
  end

  local function age(value)
    local elapsed = math.max(0, os.time() - value)
    if elapsed < 60 then return "maintenant" end
    if elapsed < 3600 then return tostring(math.floor(elapsed / 60)) .. " min" end
    if elapsed < 86400 then return tostring(math.floor(elapsed / 3600)) .. " h" end
    return tostring(math.floor(elapsed / 86400)) .. " j"
  end
  function history_publication()
    local children = { ui.row { gap = "small", children = { text("Notifications", "title"), ui.spacer { flexible = true }, ui.button { action_id = "history:" .. tostring(session) .. ":close-all", accessible_label = "Masquer toutes les notifications", content = ui.icon { name = "close" } } } } }
    local count = 0
    for _, record in ipairs(history) do
      if not hidden[record.sequence] and count < visible_count then
        local content = record.summary ~= "" and record.summary or record.body
        children[#children + 1] = ui.action_region { action_id = "history:" .. tostring(session) .. ":hide:" .. tostring(record.sequence), accessible_label = "Masquer " .. content, content = ui.column { alignment = "start", gap = "xsmall", children = { ui.row { gap = "small", children = { text(record.app_name ~= "" and record.app_name:upper() or "APPLICATION", "caption"), ui.spacer { flexible = true }, text(age(record.received_at), "caption") } }, text(content, "body") } } }
        count = count + 1
      end
    end
    if count == 0 then children[#children + 1] = text("Aucune notification", "caption") end
    return { context_id = "history", priority = 100, views = { expanded = ui.column { alignment = "start", gap = "xsmall", children = children } } }
  end
  local function close_history()
    cancel(open_timer); cancel(inactivity_timer); open_timer, inactivity_timer = nil, nil
    api.dismiss("history"); visible_count, history_was_expanded, hidden = 0, false, {}
    if api.overlay and api.overlay.close then api.overlay.close() end
  end
  local function show_more()
    if visible_count == 0 then session, hidden = session + 1, {} end
    visible_count = math.min(visible_count + 1, options.history_visible_limit); api.publish(history_publication())
    if visibility == "expanded-active" then history_was_expanded = true end
    cancel(open_timer); cancel(inactivity_timer)
    if history_was_expanded then inactivity_timer = api.timer.after(8000, close_history)
    else open_timer = api.timer.after(2000, function() if not history_was_expanded then close_history() end end) end
    return true
  end
  local function invoke(action_id)
    local hsession, operation, sequence = action_id:match("^history:(%d+):([%a%-]+):?(%d*)$")
    if hsession then
      if tonumber(hsession) ~= session or visible_count == 0 then return false end
      if operation == "close-all" then close_history(); return true end
      sequence = tonumber(sequence); hidden[sequence] = true; visible_count = visible_count - 1
      if visible_count == 0 then close_history() else api.publish(history_publication()) end
      return true
    end
    local id = tonumber(action_id:match("^notification%-(%d+):")); local notification = id and live[id]
    local route = id and routes[id] and routes[id][action_id]
    if not notification or not route then return false end
    if route[1] == "close" then return close_notification(id, 2) end
    if route[1] == "uri" then return api.uri.open(route[2]) == true end
    if route[1] == "dbus" then
      if service and service.signal then service:signal("ActionInvoked", id, route[2]) end
      if not notification.resident then close_notification(id, 2) end
      return true
    end
    return false
  end

  local function start()
    service = api.dbus.service {
      bus = "session", name = BUS_NAME, path = OBJECT_PATH, interface = INTERFACE,
      methods = {
        GetCapabilities = function() return capabilities end,
        GetServerInformation = function() return "gisland", "gisland", "1.0.0", "1.2" end,
        Notify = notify,
        CloseNotification = function(id) if not close_notification(tonumber(id), 3) then error("notification does not exist") end end,
      },
    }
  end

  return {
    start = function() local ok, message = pcall(start); if not ok then log_error(message) end end,
    actions = { ["show-more"] = show_more },
    fallback_action = invoke,
    visibility = function(value)
      visibility = value
      if value == "expanded-active" and visible_count > 0 then history_was_expanded = true; cancel(open_timer); open_timer = nil; cancel(inactivity_timer); inactivity_timer = api.timer.after(8000, close_history)
      elseif value == "hidden" and history_was_expanded then close_history() end
    end,
    stop = function()
      stopped = true; cancel(open_timer); cancel(inactivity_timer)
      for id, timer in pairs(timers) do cancel(timer); timers[id] = nil end
      if service then if service.close then service:close() elseif service.stop then service:stop() end; service = nil end
      save_history()
    end,
  }
end
