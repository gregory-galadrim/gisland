mod desktop;
mod ipc;
mod lua_runtime;
mod process;
mod ui;
mod x11;

use anyhow::{Context, Result, bail};
use ipc::{Envelope, Response};
use lua_runtime::{Command as LuaCommand, Event as LuaEvent};
use notify::{RecursiveMode, Watcher};
use raylib::prelude::*;
use serde_json::{Value, json};
use std::{
    env,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use ui::{Renderer, Visibility};

fn main() {
    if let Err(error) = entry() {
        eprintln!("gisland: {error:#}");
        std::process::exit(1);
    }
}
fn entry() -> Result<()> {
    let arguments = env::args().collect::<Vec<_>>();
    let executable = Path::new(arguments.first().map(String::as_str).unwrap_or("gisland"))
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("gisland");
    if executable == "gislandctl" || arguments.get(1).map(String::as_str) == Some("ctl") {
        let offset = if executable == "gislandctl" { 1 } else { 2 };
        std::process::exit(ipc::run_cli(&arguments[offset..])?);
    }
    run()
}

fn run() -> Result<()> {
    let _lock = ipc::acquire_lock()?;
    let asset_root = asset_root()?;
    let (event_tx, events) = mpsc::channel();
    let lua = lua_runtime::spawn(asset_root.clone(), event_tx)?;
    let (control_tx, controls) = mpsc::channel();
    ipc::start_server(control_tx)?;
    let mut watcher = watch(&asset_root, lua.clone())?;
    if let Some(config) = user_config()
        && let Some(directory) = config.parent()
        && directory.exists()
    {
        watcher.watch(directory, RecursiveMode::Recursive)?;
    }

    let mut builder = raylib::init();
    builder
        .size(472, 580)
        .title("gisland")
        .msaa_4x()
        .undecorated()
        .always_run()
        .topmost()
        .transparent()
        .unfocused()
        .hidden();
    let (mut window, thread) = builder.build();
    window.set_exit_key(None);
    let (monitor_x, monitor_y, monitor_width) = x11::primary_monitor().unwrap_or_else(|| {
        let monitor = raylib::core::window::get_current_monitor();
        let position = raylib::core::window::get_monitor_position(monitor);
        (
            position.x as i32,
            position.y as i32,
            raylib::core::window::get_monitor_width(monitor),
        )
    });
    let surface_width = monitor_width.min(472);
    window.set_window_size(surface_width, 580);
    let mut renderer = Renderer::new(asset_root.clone());
    let mut visibility = Visibility::Compact;
    let mut visibility_name = "compact-active".to_owned();
    let mut pinned = false;
    let mut input_owner: Option<String> = None;
    let mut input_had_focus = false;
    let mut input_previous_focus = None;
    let mut drun_query = desktop::QueryEditor::default();
    let mut hover_left = Instant::now();
    let mut reveal_until: Option<Instant> = None;
    let mut last_extent = renderer.extent();
    renderer.set_visibility(visibility);
    let place_window = |window: &mut RaylibHandle| {
        window.set_window_position(
            monitor_x + (monitor_width - surface_width) / 2,
            monitor_y - 12,
        );
    };
    place_window(&mut window);
    x11::shape_input(&mut window, last_extent.0, last_extent.1, true);
    x11::map_window(&mut window);
    let mut placement_frames = 30;
    let _ = lua.send(LuaCommand::Visibility(visibility_name.clone()));

    while !window.window_should_close() {
        let frame_started = Instant::now();
        if placement_frames > 0 {
            place_window(&mut window);
            placement_frames -= 1;
        }
        while let Ok(event) = events.try_recv() {
            match event {
                LuaEvent::Scene(scene) => {
                    let reveal = if scene
                        .pointer("/expanded_presentation/reveal")
                        .and_then(Value::as_str)
                        == Some("expanded")
                    {
                        Some((Visibility::Expanded, "/expanded_presentation/duration_ms"))
                    } else if scene
                        .pointer("/compact_presentation/reveal")
                        .and_then(Value::as_str)
                        == Some("compact")
                    {
                        Some((Visibility::Compact, "/compact_presentation/duration_ms"))
                    } else {
                        None
                    };
                    if let Some((target, duration_path)) = reveal {
                        let duration = scene
                            .pointer(duration_path)
                            .and_then(Value::as_u64)
                            .unwrap_or(1000);
                        reveal_until = Some(Instant::now() + Duration::from_millis(duration));
                        set_visibility(
                            &lua,
                            &mut renderer,
                            &mut visibility,
                            &mut visibility_name,
                            target,
                        );
                    }
                    if let Err(error) = renderer.set_scene(scene) {
                        eprintln!("gisland: rejected scene: {error:#}");
                    }
                }
                LuaEvent::Overlay(operation) => {
                    apply_overlay(
                        &lua,
                        &mut renderer,
                        &mut visibility,
                        &mut visibility_name,
                        &mut pinned,
                        &operation,
                    );
                    if matches!(operation.as_str(), "close" | "collapse") && input_owner.is_some() {
                        if window.is_window_focused()
                            && let Some(previous) = input_previous_focus
                        {
                            x11::restore_focus(previous);
                        }
                        input_owner = None;
                        input_had_focus = false;
                        input_previous_focus = None;
                        drun_query.clear();
                    }
                }
                LuaEvent::Dbus(call) => {
                    let _ = lua.send(LuaCommand::Dbus(call));
                }
            }
        }
        while let Ok(envelope) = controls.try_recv() {
            let command = envelope.request.command.clone();
            let opening_input = if command == "drun" {
                Some("drun".to_owned())
            } else if command == "activate-open" && envelope.request.args.len() == 1 {
                Some(envelope.request.args[0].clone())
            } else {
                None
            };
            let was_inactive = input_owner.is_none();
            if opening_input.is_some() && was_inactive {
                input_previous_focus = x11::focused_window();
            }
            let succeeded = dispatch_control(
                envelope,
                &lua,
                &mut renderer,
                &mut visibility,
                &mut visibility_name,
                &mut pinned,
            );
            if let Some(owner) = opening_input.as_ref()
                && succeeded
            {
                input_owner = Some(owner.clone());
                input_had_focus = false;
                drun_query.clear();
                x11::focus_window(&mut window);
            } else if opening_input.is_some() && was_inactive {
                if let Some(previous) = input_previous_focus.take() {
                    x11::restore_focus(previous);
                }
            } else if command == "close" && input_owner.is_some() {
                if window.is_window_focused()
                    && let Some(previous) = input_previous_focus
                {
                    x11::restore_focus(previous);
                }
                input_owner = None;
                input_had_focus = false;
                input_previous_focus = None;
                drun_query.clear();
            }
        }
        if input_owner.is_some() {
            if window.is_window_focused() {
                input_had_focus = true;
            } else if input_had_focus {
                input_owner = None;
                input_had_focus = false;
                input_previous_focus = None;
                drun_query.clear();
                apply_overlay(
                    &lua,
                    &mut renderer,
                    &mut visibility,
                    &mut visibility_name,
                    &mut pinned,
                    "close",
                );
            }
        }
        if let Some(owner) = input_owner.as_deref() {
            let repeated = |key| window.is_key_pressed(key) || window.is_key_pressed_repeat(key);
            let left = repeated(KeyboardKey::KEY_LEFT);
            let right = repeated(KeyboardKey::KEY_RIGHT);
            let up = repeated(KeyboardKey::KEY_UP);
            let down = repeated(KeyboardKey::KEY_DOWN);

            if owner == "drun" {
                let control = window.is_key_down(KeyboardKey::KEY_LEFT_CONTROL)
                    || window.is_key_down(KeyboardKey::KEY_RIGHT_CONTROL);
                let home = repeated(KeyboardKey::KEY_HOME);
                let end = repeated(KeyboardKey::KEY_END);
                let backspace = repeated(KeyboardKey::KEY_BACKSPACE);
                let delete = repeated(KeyboardKey::KEY_DELETE);
                let mut edited = false;
                let mut moved = false;

                if control && window.is_key_pressed(KeyboardKey::KEY_A) {
                    moved |= drun_query.select_all();
                }
                if left {
                    moved |= drun_query.left(control);
                }
                if right {
                    moved |= drun_query.right(control);
                }
                if home {
                    moved |= drun_query.home();
                }
                if end {
                    moved |= drun_query.end();
                }
                while let Some(character) = window.get_char_pressed() {
                    if !character.is_control() {
                        edited |= drun_query.insert(character);
                    }
                }
                if backspace {
                    edited |= drun_query.backspace(control);
                }
                if delete {
                    edited |= drun_query.delete(control);
                }
                if edited {
                    let text = drun_query.text();
                    let _ = lua.send(LuaCommand::Input {
                        kind: "query".into(),
                        value: Some(format!("{text}\0{}", drun_query.display())),
                    });
                } else if moved {
                    let _ = lua.send(LuaCommand::Input {
                        kind: "cursor".into(),
                        value: Some(drun_query.display()),
                    });
                }
            }

            if window.is_key_pressed(KeyboardKey::KEY_TAB) {
                renderer.focus_next(
                    window.is_key_down(KeyboardKey::KEY_LEFT_SHIFT)
                        || window.is_key_down(KeyboardKey::KEY_RIGHT_SHIFT),
                );
            }
            let focused = renderer.focused_hit();
            if window.is_key_pressed(KeyboardKey::KEY_ENTER)
                && let Some(hit) = focused.as_ref()
            {
                let (reply, _) = mpsc::channel();
                let _ = lua.send(LuaCommand::Action {
                    owner: hit.owner.clone(),
                    action: hit.action.clone(),
                    reply,
                });
            }
            for (triggered, kind) in [
                (up, "up"),
                (down, "down"),
                (owner != "drun" && left, "left"),
                (owner != "drun" && right, "right"),
                (
                    focused.is_none() && window.is_key_pressed(KeyboardKey::KEY_ENTER),
                    "enter",
                ),
                (window.is_key_pressed(KeyboardKey::KEY_ESCAPE), "escape"),
            ] {
                if triggered {
                    let _ = lua.send(LuaCommand::Input {
                        kind: kind.into(),
                        value: None,
                    });
                }
            }
            let wheel = window.get_mouse_wheel_move();
            if wheel != 0.0 {
                let _ = lua.send(LuaCommand::Input {
                    kind: if wheel > 0.0 { "up" } else { "down" }.into(),
                    value: None,
                });
            }
        }
        let mouse =
            x11::pointer_position(&mut window).unwrap_or_else(|| window.get_mouse_position());
        let island_width = (last_extent.0 - 40).max(1) as f32;
        let island_height = (last_extent.1 - 40).max(1) as f32;
        let island_x = (window.get_screen_width() as f32 - island_width) * 0.5;
        let inside = visibility != Visibility::Hidden
            && mouse.x >= island_x
            && mouse.y >= 20.0
            && mouse.x < island_x + island_width
            && mouse.y < 20.0 + island_height;
        if inside && visibility == Visibility::Compact && !pinned {
            set_visibility(
                &lua,
                &mut renderer,
                &mut visibility,
                &mut visibility_name,
                Visibility::Expanded,
            );
        } else if !inside
            && hover_left.elapsed() > Duration::from_millis(120)
            && visibility == Visibility::Expanded
            && !pinned
            && input_owner.is_none()
            && reveal_until.is_none_or(|deadline| Instant::now() >= deadline)
        {
            reveal_until = None;
            set_visibility(
                &lua,
                &mut renderer,
                &mut visibility,
                &mut visibility_name,
                Visibility::Compact,
            );
        } else if inside {
            hover_left = Instant::now();
        }
        if window.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
            && let Some(hit) = renderer
                .hits
                .iter()
                .rev()
                .find(|hit| hit.rectangle.check_collision_point_rec(mouse))
                .cloned()
        {
            if input_owner.is_none() {
                input_previous_focus = x11::focused_window();
                input_owner = hit.owner.clone();
                input_had_focus = false;
                x11::focus_window(&mut window);
            }
            let (reply, _) = mpsc::channel();
            let _ = lua.send(LuaCommand::Action {
                owner: hit.owner,
                action: hit.action,
                reply,
            });
        }
        let extent = if visibility == Visibility::Hidden {
            (1, 1)
        } else {
            renderer.extent()
        };
        if extent != last_extent {
            x11::shape_input(
                &mut window,
                extent.0,
                extent.1,
                visibility != Visibility::Hidden,
            );
            last_extent = extent;
        }
        renderer.prepare(&mut window, &thread);
        let mut drawing = window.begin_drawing(&thread);
        drawing.clear_background(Color::BLANK);
        renderer.draw(&mut drawing, &thread, mouse);
        drop(drawing);
        std::thread::sleep(Duration::from_micros(16_667).saturating_sub(frame_started.elapsed()));
    }
    if input_owner.is_some()
        && window.is_window_focused()
        && let Some(previous) = input_previous_focus
    {
        x11::restore_focus(previous);
    }
    let _ = lua.send(LuaCommand::Stop);
    if let Ok(path) = ipc::runtime_path("gisland.sock") {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

fn set_visibility(
    lua: &lua_runtime::Handle,
    renderer: &mut Renderer,
    visibility: &mut Visibility,
    name: &mut String,
    value: Visibility,
) {
    if *visibility == value {
        return;
    }
    *visibility = value;
    *name = match value {
        Visibility::Hidden => "hidden",
        Visibility::Compact => "compact-active",
        Visibility::Expanded => "expanded-active",
    }
    .into();
    renderer.set_visibility(value);
    let _ = lua.send(LuaCommand::Visibility(name.clone()));
}
fn apply_overlay(
    lua: &lua_runtime::Handle,
    renderer: &mut Renderer,
    visibility: &mut Visibility,
    name: &mut String,
    pinned: &mut bool,
    operation: &str,
) {
    match operation {
        "open" => {
            *pinned = true;
            set_visibility(lua, renderer, visibility, name, Visibility::Expanded);
        }
        "close" => {
            *pinned = false;
            set_visibility(lua, renderer, visibility, name, Visibility::Hidden);
        }
        "collapse" => {
            *pinned = false;
            set_visibility(lua, renderer, visibility, name, Visibility::Compact);
        }
        "toggle" if *visibility == Visibility::Hidden => {
            *pinned = true;
            set_visibility(lua, renderer, visibility, name, Visibility::Expanded);
        }
        "toggle" => {
            *pinned = false;
            set_visibility(lua, renderer, visibility, name, Visibility::Hidden);
        }
        _ => {}
    }
}

fn dispatch_control(
    envelope: Envelope,
    lua: &lua_runtime::Handle,
    renderer: &mut Renderer,
    visibility: &mut Visibility,
    name: &mut String,
    pinned: &mut bool,
) -> bool {
    let request = envelope.request;
    let response = match request.command.as_str() {
        "open" => {
            apply_overlay(lua, renderer, visibility, name, pinned, "open");
            ipc::ok(None)
        }
        "close" => {
            apply_overlay(lua, renderer, visibility, name, pinned, "close");
            ipc::ok(None)
        }
        "toggle" => {
            apply_overlay(lua, renderer, visibility, name, pinned, "toggle");
            ipc::ok(None)
        }
        "drun" => simple_lua(lua, |reply| LuaCommand::Activate {
            instance: "drun".into(),
            open: true,
            reply,
        }),
        "status" => match lua_status(lua) {
            Ok(value) => ipc::ok(Some(ipc::basic_status(name, value))),
            Err(error) => ipc::failure(error),
        },
        "modules" => match lua_status(lua) {
            Ok(value) => ipc::ok(Some(
                value.get("modules").cloned().unwrap_or_else(|| json!([])),
            )),
            Err(error) => ipc::failure(error),
        },
        "reload" => simple_lua(lua, LuaCommand::Reload),
        "module-restart" => one_argument(&request.args).map_or_else(ipc::failure, |instance| {
            simple_lua(lua, |reply| LuaCommand::Restart { instance, reply })
        }),
        "activate" | "activate-open" => {
            one_argument(&request.args).map_or_else(ipc::failure, |instance| {
                let open = request.command == "activate-open";
                if open {
                    apply_overlay(lua, renderer, visibility, name, pinned, "open");
                }
                simple_lua(lua, |reply| LuaCommand::Activate {
                    instance,
                    open,
                    reply,
                })
            })
        }
        "dismiss" => one_argument(&request.args).map_or_else(ipc::failure, |context| {
            simple_lua(lua, |reply| LuaCommand::Dismiss { context, reply })
        }),
        "action" => {
            let (owner, action) = match request.args.as_slice() {
                [action] => (None, action.clone()),
                [owner, action] => (Some(owner.clone()), action.clone()),
                _ => {
                    let _ = envelope
                        .reply
                        .send(ipc::failure("usage: gislandctl action [instance] <action>"));
                    return false;
                }
            };
            simple_lua(lua, |reply| LuaCommand::Action {
                owner,
                action,
                reply,
            })
        }
        other => ipc::failure(format!("unknown command: {other}")),
    };
    let succeeded = response.ok;
    let _ = envelope.reply.send(response);
    succeeded
}
fn simple_lua(
    lua: &lua_runtime::Handle,
    make: impl FnOnce(mpsc::Sender<Result<()>>) -> LuaCommand,
) -> Response {
    let (reply, receiver) = mpsc::channel();
    if let Err(error) = lua.send(make(reply)) {
        return ipc::failure(error);
    }
    match receiver.recv_timeout(Duration::from_secs(8)) {
        Ok(Ok(())) => ipc::ok(None),
        Ok(Err(error)) => ipc::failure(error),
        Err(error) => ipc::failure(error),
    }
}
fn lua_status(lua: &lua_runtime::Handle) -> Result<Value> {
    let (reply, receiver) = mpsc::channel();
    lua.send(LuaCommand::Status(reply))?;
    receiver
        .recv_timeout(Duration::from_secs(3))
        .context("Lua status timed out")?
}
fn one_argument(arguments: &[String]) -> Result<String> {
    if let [value] = arguments {
        Ok(value.clone())
    } else {
        bail!("command requires one argument")
    }
}
fn asset_root() -> Result<PathBuf> {
    if let Some(path) = env::var_os("GISLAND_ASSET_ROOT").map(PathBuf::from) {
        return Ok(path);
    }
    let executable = env::current_exe()?;
    if let Some(prefix) = executable.parent().and_then(Path::parent) {
        let installed = prefix.join("share/gisland");
        if installed.join("init.lua").is_file() {
            return Ok(installed);
        }
    }
    let development = env::current_dir()?.join("assets");
    if development.join("init.lua").is_file() {
        return Ok(development);
    }
    bail!("cannot find gisland Lua assets; set GISLAND_ASSET_ROOT")
}
fn user_config() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|root| root.join("gisland/init.lua"))
}
fn watch(asset_root: &Path, lua: lua_runtime::Handle) -> Result<notify::RecommendedWatcher> {
    let last = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if !matches!(
            event.kind,
            notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_)
        ) {
            return;
        }
        let mut last = last.lock().unwrap();
        if last.elapsed() < Duration::from_millis(150) {
            return;
        }
        *last = Instant::now();
        let (reply, _) = mpsc::channel();
        let _ = lua.send(LuaCommand::Reload(reply));
    })?;
    watcher.watch(asset_root, RecursiveMode::Recursive)?;
    Ok(watcher)
}
