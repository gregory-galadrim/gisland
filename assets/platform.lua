local platform = {}

function platform.host(candidate)
  return candidate or rawget(_G, "api") or rawget(_G, "gisland_api")
end

function platform.copy_options(defaults, supplied, name)
  supplied = supplied or {}
  local result = {}
  local unknown
  for key in pairs(supplied) do
    if defaults[key] == nil and unknown == nil then unknown = key end
  end
  if unknown ~= nil then error("unknown " .. name .. " option: " .. unknown) end
  for key, default in pairs(defaults) do
    result[key] = supplied[key]
    if result[key] == nil then result[key] = default end
  end
  return result
end

function platform.integer(value, name, minimum, maximum)
  if type(value) ~= "number" or value ~= math.floor(value) then
    error(name .. " must be an integer")
  end
  if minimum ~= nil and value < minimum or maximum ~= nil and value > maximum then
    error(name .. " is out of range")
  end
  return value
end

function platform.round(value)
  local lower = math.floor(value)
  local fraction = value - lower
  if fraction < 0.5 then return lower end
  if fraction > 0.5 then return lower + 1 end
  return lower % 2 == 0 and lower or lower + 1
end

function platform.clamp(value, minimum, maximum)
  if value < minimum then return minimum end
  if value > maximum then return maximum end
  return value
end

function platform.finite(value)
  return type(value) == "number" and value == value and value ~= math.huge and value ~= -math.huge
end

function platform.trim(value)
  return (value:gsub("^%s*(.-)%s*$", "%1"))
end

function platform.after(api, milliseconds, callback)
  return api.timer.after(milliseconds, callback)
end

function platform.defer(api, callback)
  return api.timer.defer(callback)
end

function platform.publish(api, spec)
  return api.publish(spec)
end

function platform.dismiss(api, id)
  return api.dismiss(id)
end

return platform
