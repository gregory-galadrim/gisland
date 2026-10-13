package.path = "assets/?.lua;assets/?/init.lua;" .. package.path
local scene, overlay
package.preload["focus-fixture"] = function()
  return function(api)
    return { start=function()
      api.publish { context_id="clock", priority=10, views={compact={type="text",value="14:32"}, expanded={type="text",value="Calendar"}} }
    end }
  end
end
local native={api_version=1, log=function() end, cleanup=function() end,
  present=function(value) scene=value end, overlay=function(value) overlay=value end,
  owner=function() return {timer={after=function() end,defer=function(f) f() end}} end}
local runtime=require("runtime").start(native,{api=1,defaults={compact="clock",expanded="clock"},modules={{id="clock",source="focus-fixture"}}})
assert(runtime.activate("clock",true))
assert(runtime.input("escape"))
assert(overlay=="collapse", "Escape must collapse the focused island")
assert(scene.compact_context=="clock" and scene.expanded_context=="clock", "Escape must preserve the default calendar")
print("runtime focus check passed")
