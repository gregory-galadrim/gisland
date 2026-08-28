return function(api, supplied)
  if supplied and next(supplied) ~= nil then error("drun has no options") end
  local query, display, selected, results = "", "", 1, {}

  local function close()
    api.overlay.close()
    api.dismiss("drun")
    return true
  end

  local function publish()
    local first = math.max(1, math.min(selected - 5, #results - 5))
    local last = math.min(first + 5, #results)
    local children = {
      api.ui.action_region { style = "input", action_id = "focus-query", accessible_label = "Search applications",
        content = api.ui.text { value = query == "" and "Search applications…" or display,
          role = query == "" and "input-muted" or "body" } },
    }
    for index = first, last do local application = results[index]
      local icon = application.icon and api.ui.image {
        source = api.image.load(application.icon), accessible_label = application.name,
      } or api.ui.icon { name = "application", role = index == selected and "accent" or "muted" }
      children[#children + 1] = api.ui.action_region {
        action_id = "launch:" .. tostring(index), accessible_label = "Launch " .. application.name,
        content = api.ui.row { gap = "small", children = {
          icon,
          api.ui.text { value = application.name, role = index == selected and "accent" or "body" },
        } },
      }
    end
    if #results == 0 then
      children[#children + 1] = api.ui.spacer {}
      children[#children + 1] = api.ui.text { value = "No applications found", role = "muted" }
      children[#children + 1] = api.ui.spacer {}
    end
    api.publish {
      context_id = "drun", priority = 1000,
      views = {
        compact = api.ui.text { value = "Applications", role = "compact-primary" },
        expanded = api.ui.column { alignment = "start", gap = "xsmall", children = children },
      },
    }
  end

  local function refresh()
    results = api.desktop.search(query, 512)
    if #results == 0 then selected = 0 else selected = math.max(1, math.min(selected, #results)) end
    publish()
  end

  local function launch(index)
    local application = results[index]
    if not application then return false end
    local ok, launched = pcall(api.desktop.launch, application.id)
    if not ok or launched ~= true then
      api.log("error", "could not launch " .. application.name .. ": " .. tostring(launched))
      return false
    end
    return close()
  end

  local function input(kind, value)
    if kind == "query" then local split = value:find("\0", 1, true); query, display, selected = value:sub(1, split - 1), value:sub(split + 1), 1; refresh(); return true end
    if kind == "cursor" then display = value; publish(); return true end
    if kind == "up" and selected > 1 then selected = selected - 1; publish(); return true end
    if kind == "down" and selected < #results then selected = selected + 1; publish(); return true end
    if kind == "enter" then return launch(selected) end
    if kind == "escape" then return close() end
    return false
  end

  return {
    activate = function() query, display, selected = "", "", 1; refresh() end,
    input = input,
    fallback_action = function(action_id) if action_id == "focus-query" then return true end; local index = tonumber(action_id:match("^launch:(%d+)$")); return index and launch(index) or false end,
    visibility = function(value) if value == "hidden" then api.dismiss("drun") end end,
  }
end
