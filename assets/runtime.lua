local runtime = {}

local function copy(value)
  if type(value) ~= "table" then return value end
  local result = {}
  for key, item in pairs(value) do result[key] = copy(item) end
  return result
end

function runtime.start(native, config)
  assert(config.api == native.api_version, "unsupported Lua API version")
  local self = { native = native, config = config, modules = {}, contexts = {}, sequence = 0,
    visibility_state = "compact", forced = nil }

  local function log_error(record, where, message)
    native.log("error", record.id .. " " .. where .. ": " .. tostring(message))
  end

  local function remove_owner(record)
    for key, context in pairs(self.contexts) do if context.owner == record.id then self.contexts[key] = nil end end
    native.cleanup(record.id)
  end

  local function fail(record, where, message)
    if record.state == "failed" then return end
    log_error(record, where, message)
    record.state, record.error = "failed", tostring(message)
    remove_owner(record)
  end

  local function guard(record, where, callback, ...)
    if record == nil or record.state ~= "running" and where ~= "start" then return nil end
    local result = { pcall(callback, ...) }
    if not table.remove(result, 1) then fail(record, where, result[1]) return nil end
    return unpack(result)
  end

  local function active(surface)
    local winner
    for _, context in pairs(self.contexts) do
      if context.views and context.views[surface] and (self.forced == nil or context.owner == self.forced) and
          (winner == nil or context.priority > winner.priority or context.priority == winner.priority and context.sequence > winner.sequence) then
        winner = context
      end
    end
    if winner == nil and self.forced ~= nil then self.forced = nil return active(surface) end
    if winner == nil then
      local wanted = self.config.defaults and self.config.defaults[surface]
      for _, context in pairs(self.contexts) do
        if context.owner == wanted and context.views and context.views[surface] and
            (winner == nil or context.sequence > winner.sequence) then winner = context end
      end
    end
    return winner
  end

  local function present()
    local compact, expanded = active("compact"), active("expanded")
    native.present {
      compact = compact and compact.views.compact or nil,
      expanded = expanded and expanded.views.expanded or nil,
      compact_owner = compact and compact.owner or nil, expanded_owner = expanded and expanded.owner or nil,
      compact_context = compact and compact.context_id or nil, expanded_context = expanded and expanded.context_id or nil,
      compact_presentation = compact and compact.presentation or nil,
      expanded_presentation = expanded and expanded.presentation or nil,
      compact_transitions = compact and compact.transitions or nil,
      expanded_transitions = expanded and expanded.transitions or nil,
      resources = expanded and expanded.resources or compact and compact.resources or nil,
      theme = self.config.theme, interaction = self.config.interaction,
    }
  end

  local function scoped_api(record)
    local api = native.owner(record.id)
    local base_after, base_defer = api.timer.after, api.timer.defer
    api.timer.after = function(milliseconds, callback)
      return base_after(milliseconds, function() guard(record, "timer", callback) present() end)
    end
    api.timer.defer = function(callback)
      return base_defer(function() guard(record, "deferred callback", callback) present() end)
    end
    local base_service = api.dbus and api.dbus.service
    if base_service then
      api.dbus.service = function(spec)
        local wrapped = copy(spec); wrapped.methods = {}
        for name, callback in pairs(spec.methods or {}) do
          wrapped.methods[name] = function(...) return guard(record, "D-Bus " .. name, callback, ...) end
        end
        return base_service(wrapped)
      end
    end
    api.publish = function(spec)
      assert(type(spec) == "table" and type(spec.context_id) == "string", "publication requires context_id")
      assert(type(spec.views) == "table", "publication requires views")
      self.sequence = self.sequence + 1
      local key = record.id .. "\0" .. spec.context_id
      self.contexts[key] = { owner = record.id, context_id = spec.context_id, priority = tonumber(spec.priority) or 0,
        sequence = self.sequence, views = copy(spec.views), presentation = copy(spec.presentation), transitions = copy(spec.transitions), resources = copy(spec.resources) }
      if spec.expires_in_ms and spec.expires_in_ms > 0 then
        local sequence = self.sequence
        api.timer.after(spec.expires_in_ms, function()
          local current = self.contexts[key]
          if current and current.sequence == sequence then self.contexts[key] = nil present() end
        end)
      end
      present()
    end
    api.dismiss = function(context_id) self.contexts[record.id .. "\0" .. context_id] = nil present() end
    return api
  end

  local function start_record(definition)
    local record = { id = assert(definition.id, "module id is required"), source = assert(definition.source, "module source is required"),
      options = definition.options or {}, enabled = definition.enabled ~= false, state = "disabled" }
    self.modules[#self.modules + 1] = record
    if not record.enabled then return record end
    local ok, factory = pcall(require, record.source)
    if not ok or type(factory) ~= "function" then
      record.state, record.error = "failed", tostring(factory) log_error(record, "load", factory) return record
    end
    record.factory = factory
    local api = scoped_api(record)
    local made, instance = pcall(factory, api, copy(record.options))
    if not made or type(instance) ~= "table" then
      record.state, record.error = "failed", tostring(instance) log_error(record, "factory", instance) return record
    end
    record.api, record.instance, record.state = api, instance, "running"
    if instance.start then guard(record, "start", instance.start) end
    return record
  end

  local function find_module(id)
    for _, record in ipairs(self.modules) do if record.id == id then return record end end
  end

  function self.restart(id)
    local old = find_module(id)
    if not old then return false, "unknown module: " .. tostring(id) end
    if old.instance and old.instance.stop then guard(old, "stop", old.instance.stop) end
    remove_owner(old)
    local definition = { id = old.id, source = old.source, enabled = true, options = old.options }
    for index, record in ipairs(self.modules) do if record == old then table.remove(self.modules, index) break end end
    local fresh = start_record(definition) present()
    return fresh.state == "running", fresh.error
  end

  function self.action(owner, action_id)
    local function invoke(record)
      if record == nil or record.state ~= "running" then return nil end
      local callback = record.instance.actions and record.instance.actions[action_id]
      if callback then return guard(record, "action " .. action_id, callback) end
      if record.instance.fallback_action then return guard(record, "action " .. action_id, record.instance.fallback_action, action_id) end
    end
    if owner then
      local result = invoke(find_module(owner)) present()
      return result == true, result == true and nil or "action was not handled"
    end
    for _, record in ipairs(self.modules) do if invoke(record) == true then present() return true end end
    present() return false, "action was not handled"
  end

  function self.activate(id, open)
    local record = find_module(id)
    if not record or record.state ~= "running" then return false, "module is not running" end
    self.forced = id
    if open then native.overlay("open") end
    present() return true
  end

  function self.dismiss(context_id)
    local removed = false
    for key, context in pairs(self.contexts) do
      if context.context_id == context_id then self.contexts[key], removed = nil, true end
    end
    present() return removed, removed and nil or "unknown context"
  end

  function self.visibility(value)
    self.visibility_state = value
    for _, record in ipairs(self.modules) do
      if record.state == "running" and record.instance.visibility then guard(record, "visibility", record.instance.visibility, value) end
    end
    present()
  end

  function self.shutdown()
    for _, record in ipairs(self.modules) do
      if record.state == "running" and record.instance and record.instance.stop then guard(record, "stop", record.instance.stop) end
      remove_owner(record)
      if record.state == "running" then record.state = "disabled" end
    end
    self.contexts = {}
  end

  function self.status()
    local modules = {}
    for _, record in ipairs(self.modules) do
      modules[#modules + 1] = { instance = record.id, module = record.source, state = record.state,
        enabled = record.enabled, error = record.error }
    end
    local compact, expanded = active("compact"), active("expanded")
    return { modules = modules, compact_context = compact and compact.context_id,
      expanded_context = expanded and expanded.context_id, visibility = self.visibility_state }
  end

  for _, definition in ipairs(config.modules or {}) do start_record(definition) end
  present()
  return self
end

return runtime
