package.path = "assets/?.lua;assets/?/init.lua;" .. package.path
local theme = require("theme")
local scenes, published, publications, methods = {}, nil, {}, nil
local battery = { IsPresent=true, Percentage=89, State=2, TimeToEmpty=11760,
  TimeToFull=0, EnergyFull=48, EnergyFullDesign=50, EnergyRate=12.4, OnBattery=true }
local api = { ui = {}, timer = { after=function() end, defer=function(f) f() end, cancel=function() end },
  publish = function(spec) published=spec publications[spec.context_id]=spec end,
  dismiss=function() end, overlay={close=function() end}, log=function() end,
  state={load=function(_, default) return default end, save=function() end},
  time={ now=function() return { year=2026,month=10,day=13,second=0 } end,
    format=function(_, pattern) return ({ ["%H:%M"]="14:32", ["%a %d %b"]="Tue 13 Oct", ["%B"]="October", ["%A %d %B"]="Tuesday 13 October" })[pattern] or "13 October" end },
  process={ run=function(argv) return {ok=true, output=argv[2]=="get-sink-volume" and " / 60%" or "Mute: no"} end },
  dbus={ proxy=function() return { get=function(_, name) return battery[name] end } end,
    service=function(spec) methods=spec.methods return {signal=function() end} end },
  image={}, uri={open=function() return true end},
}
for _, kind in ipairs { "text", "rich_text", "row", "column", "spacer", "button", "action_region", "icon", "progress", "image", "divider" } do
  api.ui[kind] = function(value) value.type=kind return value end
end
local function capture(name, surface, spec)
  spec=spec or published
  scenes[#scenes+1]={name=name, surface=surface, theme=theme, interaction={reduced_motion=true},
    compact=spec.views.compact, expanded=spec.views.expanded,
    compact_context=spec.context_id, expanded_context=spec.context_id,
    compact_presentation=spec.presentation, expanded_presentation=spec.presentation}
end
local clock=require("modules.clock-calendar.clock_calendar")(api,{})
clock.start()
capture("clock", "compact")
capture("calendar", "expanded")
local audio=require("modules.audio.audio")(api,{})
audio.actions["volume-up"]()
capture("volume", "compact")
audio.actions["toggle-mute"]()
capture("mute", "compact")
local power=require("modules.power-menu.power_menu")(api,{})
power.activate()
capture("session", "expanded")
power.actions["power-off"]()
capture("confirmation", "expanded")
local batt=require("modules.battery.battery")(api,{})
batt.start()
capture("battery", "compact", publications.battery)
capture("battery-detail", "expanded", publications.battery)
for _, level in ipairs {20,5} do
  battery.Percentage=level
  local instance=require("modules.battery.battery")(api,{})
  instance.start()
  capture("battery-"..level,"compact",publications.battery)
  if level==5 then capture("battery-alert","expanded") end
end
local notifications=require("modules.notifications.notifications")(api,{})
notifications.start()
methods.Notify("Files",0,"","Download complete","The gisland.tar.gz archive is ready in Downloads.",{"open","Open folder"},{},0)
capture("notification", "expanded")
capture("notification-compact", "compact")
notifications.actions["show-more"]()
capture("history", "expanded")
local short_id=methods.Notify("Terminal",0,"","Command complete","",{},{},0)
capture("notification-small", "expanded", publications["notification-"..short_id])
local empty=require("modules.notifications.notifications")(api,{})
empty.actions["show-more"]()
capture("history-empty","expanded")
api.desktop={ search=function() local rows={} for i=1,6 do rows[i]={id=tostring(i),name="Application "..i} end return rows end }
local drun=require("modules.drun.drun")(api,{})
drun.activate()
capture("drun","expanded")
return scenes
