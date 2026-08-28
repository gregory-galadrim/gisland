return {
  api = 1,
  theme = require("theme"),
  defaults = { compact = "clock", expanded = "clock" },
  interaction = { animation_speed = 1.25, hover_exit_ms = 120, reduced_motion = false },
  modules = {
    { id = "clock", source = "modules.clock-calendar.clock_calendar", enabled = true,
      options = { week_start = "monday" } },
    { id = "drun", source = "modules.drun.drun", enabled = true, options = {} },
    { id = "power-menu", source = "modules.power-menu.power_menu", enabled = true, options = {} },
    { id = "notifications", source = "modules.notifications.notifications", enabled = true, options = {} },
    { id = "battery", source = "modules.battery.battery", enabled = true, options = {} },
    { id = "audio", source = "modules.audio.audio", enabled = true,
      options = { step_percent = 5, maximum_percent = 150, hud_duration_ms = 1500 } },
    { id = "lua-example", source = "modules.lua-example.example", enabled = false, options = {} },
  },
}
