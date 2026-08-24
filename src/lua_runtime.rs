use crate::process;
use anyhow::{Context, Result, bail};
use chrono::{Datelike, Local, Locale, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use mlua::{Function, Lua, LuaOptions, LuaSerdeExt, MultiValue, RegistryKey, StdLib, Table, Value};
use serde_json::Value as Json;
use std::{
    collections::HashMap,
    env, fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub struct DbusCall {
    pub service: u64,
    pub method: String,
    pub arguments: Json,
    pub reply: mpsc::Sender<std::result::Result<Json, String>>,
}
pub enum Event {
    Scene(Json),
    Overlay(String),
    Dbus(DbusCall),
}
pub enum Command {
    Action {
        owner: Option<String>,
        action: String,
        reply: mpsc::Sender<Result<()>>,
    },
    Activate {
        instance: String,
        open: bool,
        reply: mpsc::Sender<Result<()>>,
    },
    Dismiss {
        context: String,
        reply: mpsc::Sender<Result<()>>,
    },
    Restart {
        instance: String,
        reply: mpsc::Sender<Result<()>>,
    },
    Visibility(String),
    Status(mpsc::Sender<Result<Json>>),
    Reload(mpsc::Sender<Result<()>>),
    Dbus(DbusCall),
    Stop,
}
#[derive(Clone)]
pub struct Handle {
    sender: mpsc::Sender<Command>,
}
impl Handle {
    pub fn send(&self, command: Command) -> Result<()> {
        self.sender.send(command).context("Lua runtime stopped")
    }
}

struct Timer {
    id: u64,
    owner: String,
    due: Instant,
    callback: RegistryKey,
}
static NEXT_TIMER: AtomicU64 = AtomicU64::new(1);
struct ServiceCallback {
    owner: String,
    callback: RegistryKey,
}
struct App {
    lua: Lua,
    runtime: Table,
    timers: Arc<Mutex<Vec<Timer>>>,
    services: Arc<Mutex<HashMap<(u64, String), ServiceCallback>>>,
}

pub fn spawn(asset_root: PathBuf, events: mpsc::Sender<Event>) -> Result<Handle> {
    let (sender, receiver) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("gisland-lua".into())
        .spawn(move || {
            let mut app = match App::load(&asset_root, events.clone()) {
                Ok(app) => {
                    let _ = ready_tx.send(Ok(()));
                    app
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("{error:#}")));
                    return;
                }
            };
            loop {
                match receiver.recv_timeout(app.timeout()) {
                    Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(Command::Reload(reply)) => {
                        let result =
                            App::load(&asset_root, events.clone()).map(|fresh| app = fresh);
                        let _ = reply.send(result.map(|_| ()));
                    }
                    Ok(command) => app.command(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                app.run_timers();
            }
        })?;
    ready_rx
        .recv()
        .context("Lua runtime did not start")?
        .map_err(anyhow::Error::msg)?;
    Ok(Handle { sender })
}

impl App {
    fn load(asset_root: &Path, events: mpsc::Sender<Event>) -> Result<Self> {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
        restrict(&lua, asset_root)?;
        let timers = Arc::new(Mutex::new(Vec::new()));
        let services = Arc::new(Mutex::new(HashMap::new()));
        let native = native_table(&lua, asset_root, events, timers.clone(), services.clone())?;
        let config_path = config_path(asset_root);
        let config_source = fs::read_to_string(&config_path)
            .with_context(|| format!("cannot read {}", config_path.display()))?;
        let config: Table = lua
            .load(config_source)
            .set_name(config_path.to_string_lossy())
            .eval()
            .context("invalid Lua configuration")?;
        let runtime_source = fs::read_to_string(asset_root.join("runtime.lua"))?;
        let module: Table = lua.load(runtime_source).set_name("runtime.lua").eval()?;
        let start: Function = module.get("start")?;
        let runtime = start
            .call((native, config))
            .context("cannot start Lua platform")?;
        Ok(Self {
            lua,
            runtime,
            timers,
            services,
        })
    }

    fn timeout(&self) -> Duration {
        self.timers
            .lock()
            .unwrap()
            .iter()
            .map(|timer| timer.due.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or(Duration::from_millis(250))
            .min(Duration::from_millis(250))
    }

    fn run_timers(&self) {
        let now = Instant::now();
        let mut due = Vec::new();
        {
            let mut timers = self.timers.lock().unwrap();
            let mut index = 0;
            while index < timers.len() {
                if timers[index].due <= now {
                    due.push(timers.remove(index));
                } else {
                    index += 1;
                }
            }
        }
        for timer in due {
            if let Err(error) = self
                .lua
                .registry_value::<Function>(&timer.callback)
                .and_then(|callback| callback.call::<()>(()))
            {
                eprintln!("gisland: {} timer failed: {error}", timer.owner);
            }
            let _ = self.lua.remove_registry_value(timer.callback);
        }
    }

    fn command(&self, command: Command) {
        match command {
            Command::Action {
                owner,
                action,
                reply,
            } => {
                let result = (|| {
                    let function: Function = self.runtime.get("action")?;
                    bool_result(function.call((owner, action))?)
                })();
                let _ = reply.send(result);
            }
            Command::Activate {
                instance,
                open,
                reply,
            } => {
                let result = (|| {
                    let function: Function = self.runtime.get("activate")?;
                    bool_result(function.call((instance, open))?)
                })();
                let _ = reply.send(result);
            }
            Command::Dismiss { context, reply } => {
                let result = (|| {
                    let function: Function = self.runtime.get("dismiss")?;
                    bool_result(function.call(context)?)
                })();
                let _ = reply.send(result);
            }
            Command::Restart { instance, reply } => {
                let result = (|| {
                    let function: Function = self.runtime.get("restart")?;
                    bool_result(function.call(instance)?)
                })();
                let _ = reply.send(result);
            }
            Command::Visibility(value) => {
                if let Ok(function) = self.runtime.get::<Function>("visibility")
                    && let Err(error) = function.call::<()>(value)
                {
                    eprintln!("gisland: visibility callback failed: {error}");
                }
            }
            Command::Status(reply) => {
                let _ = reply.send(self.status_json());
            }
            Command::Dbus(call) => {
                let result = self.invoke_dbus(&call);
                let _ = call
                    .reply
                    .send(result.map_err(|error| format!("{error:#}")));
            }
            Command::Reload(_) | Command::Stop => {}
        }
    }

    fn status_json(&self) -> Result<Json> {
        let function: Function = self.runtime.get("status")?;
        let status: Table = function.call(())?;
        let modules: Table = status.get("modules")?;
        let mut values = Vec::new();
        for module in modules.sequence_values::<Table>() {
            let module = module?;
            values.push(serde_json::json!({
                "instance": lua_text(module.get::<Value>("instance")?).unwrap_or_default(),
                "module": lua_text(module.get::<Value>("module")?).unwrap_or_default(),
                "state": lua_text(module.get::<Value>("state")?).unwrap_or_else(|| "failed".into()),
                "enabled": module.get::<bool>("enabled").unwrap_or(false),
                "error": lua_text(module.get::<Value>("error")?),
            }));
        }
        Ok(serde_json::json!({
            "modules": values,
            "compact_context": lua_text(status.get::<Value>("compact_context")?),
            "expanded_context": lua_text(status.get::<Value>("expanded_context")?),
            "visibility": lua_text(status.get::<Value>("visibility")?).unwrap_or_else(|| "hidden".into()),
        }))
    }

    fn invoke_dbus(&self, call: &DbusCall) -> Result<Json> {
        let services = self.services.lock().unwrap();
        let entry = services
            .get(&(call.service, call.method.clone()))
            .ok_or_else(|| anyhow::anyhow!("unknown D-Bus method {}", call.method))?;
        let callback: Function = self.lua.registry_value(&entry.callback)?;
        drop(services);
        let values = call
            .arguments
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|value| self.lua.to_value(&value))
            .collect::<mlua::Result<Vec<_>>>()?;
        let returned: MultiValue = callback.call(MultiValue::from_vec(values))?;
        let mut json = returned
            .into_vec()
            .into_iter()
            .map(|value| self.lua.from_value(value))
            .collect::<mlua::Result<Vec<Json>>>()?;
        Ok(match json.len() {
            0 => Json::Null,
            1 => json.remove(0),
            _ => Json::Array(json),
        })
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Ok(shutdown) = self.runtime.get::<Function>("shutdown") {
            let _ = shutdown.call::<()>(());
        }
    }
}

fn lua_text(value: Value) -> Option<String> {
    match value {
        Value::String(value) => value.to_str().ok().map(|value| value.to_owned()),
        Value::Integer(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn bool_result((ok, error): (bool, Option<String>)) -> Result<()> {
    if ok {
        Ok(())
    } else {
        bail!(error.unwrap_or_else(|| "Lua operation failed".into()))
    }
}

fn restrict(lua: &Lua, asset_root: &Path) -> Result<()> {
    let package: Table = lua.globals().get("package")?;
    let mut roots = vec![asset_root.to_owned()];
    if let Some(root) = config_home() {
        roots.insert(0, root);
    }
    let paths = roots
        .iter()
        .flat_map(|root| {
            [
                format!("{}/?.lua", root.display()),
                format!("{}/?/init.lua", root.display()),
            ]
        })
        .collect::<Vec<_>>();
    package.set("path", paths.join(";"))?;
    package.set("cpath", "")?;
    package.set("loadlib", Value::Nil)?;
    let loaders: Table = package
        .get("loaders")
        .or_else(|_| package.get("searchers"))?;
    for index in 3..=loaders.raw_len() {
        loaders.raw_set(index, Value::Nil)?;
    }
    if let Ok(io) = lua.globals().get::<Table>("io") {
        io.set("popen", Value::Nil)?;
    }
    if let Ok(os) = lua.globals().get::<Table>("os") {
        os.set("execute", Value::Nil)?;
    }
    lua.globals().set("dofile", Value::Nil)?;
    lua.globals().set("loadfile", Value::Nil)?;
    Ok(())
}

fn config_home() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|root| root.join("gisland"))
}
fn config_path(asset_root: &Path) -> PathBuf {
    config_home()
        .map(|root| root.join("init.lua"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| asset_root.join("init.lua"))
}

fn native_table(
    lua: &Lua,
    asset_root: &Path,
    events: mpsc::Sender<Event>,
    timers: Arc<Mutex<Vec<Timer>>>,
    services: Arc<Mutex<HashMap<(u64, String), ServiceCallback>>>,
) -> mlua::Result<Table> {
    let native = lua.create_table()?;
    native.set("api_version", 1)?;
    native.set("ui", ui_table(lua)?)?;
    native.set(
        "log",
        lua.create_function(|_, (level, message): (String, String)| {
            eprintln!("gisland [{level}]: {message}");
            Ok(())
        })?,
    )?;
    let present_events = events.clone();
    native.set(
        "present",
        lua.create_function(move |lua, scene: Table| {
            let value = lua.from_value(Value::Table(scene))?;
            present_events
                .send(Event::Scene(value))
                .map_err(mlua::Error::external)
        })?,
    )?;
    let overlay_events = events.clone();
    native.set(
        "overlay",
        lua.create_function(move |_, operation: String| {
            overlay_events
                .send(Event::Overlay(operation))
                .map_err(mlua::Error::external)
        })?,
    )?;
    let cleanup_timers = timers.clone();
    let cleanup_services = services.clone();
    native.set(
        "cleanup",
        lua.create_function(move |lua, owner: String| {
            let mut values = cleanup_timers.lock().unwrap();
            let mut index = 0;
            while index < values.len() {
                if values[index].owner == owner {
                    let timer = values.remove(index);
                    lua.remove_registry_value(timer.callback)?;
                } else {
                    index += 1;
                }
            }
            let keys = cleanup_services
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, callback)| callback.owner == owner)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(callback) = cleanup_services.lock().unwrap().remove(&key) {
                    lua.remove_registry_value(callback.callback)?;
                }
            }
            Ok(())
        })?,
    )?;
    let root = asset_root.to_owned();
    native.set(
        "owner",
        lua.create_function(move |lua, owner: String| {
            owner_api(
                lua,
                &owner,
                &root,
                timers.clone(),
                events.clone(),
                services.clone(),
            )
        })?,
    )?;
    Ok(native)
}

fn ui_table(lua: &Lua) -> mlua::Result<Table> {
    let ui = lua.create_table()?;
    for kind in [
        "text",
        "rich_text",
        "icon",
        "image",
        "row",
        "column",
        "spacer",
        "progress",
        "indicator",
        "button",
        "action_region",
    ] {
        let name = kind.to_owned();
        ui.set(
            kind,
            lua.create_function(move |_, value: Table| {
                value.set("type", name.as_str())?;
                Ok(value)
            })?,
        )?;
    }
    Ok(ui)
}

fn owner_api(
    lua: &Lua,
    owner: &str,
    asset_root: &Path,
    timers: Arc<Mutex<Vec<Timer>>>,
    events: mpsc::Sender<Event>,
    services: Arc<Mutex<HashMap<(u64, String), ServiceCallback>>>,
) -> mlua::Result<Table> {
    let api = lua.create_table()?;
    api.set("id", owner)?;
    api.set("api_version", 1)?;
    api.set("ui", ui_table(lua)?)?;
    let timer = lua.create_table()?;
    for (name, delay) in [("after", true), ("defer", false)] {
        let values = timers.clone();
        let owner = owner.to_owned();
        if delay {
            timer.set(
                name,
                lua.create_function(move |lua, (milliseconds, callback): (u64, Function)| {
                    if milliseconds > 86_400_000 {
                        return Err(mlua::Error::external("timer exceeds one day"));
                    }
                    add_timer(lua, &values, &owner, milliseconds, callback)
                })?,
            )?;
        } else {
            timer.set(
                name,
                lua.create_function(move |lua, callback: Function| {
                    add_timer(lua, &values, &owner, 0, callback)
                })?,
            )?;
        }
    }
    let cancel = timers.clone();
    timer.set(
        "cancel",
        lua.create_function(move |lua, id: u64| {
            let index = cancel
                .lock()
                .unwrap()
                .iter()
                .position(|timer| timer.id == id);
            if let Some(index) = index {
                let timer = cancel.lock().unwrap().remove(index);
                lua.remove_registry_value(timer.callback)?;
                Ok(true)
            } else {
                Ok(false)
            }
        })?,
    )?;
    api.set("timer", timer)?;
    let processes = lua.create_table()?;
    processes.set("run", lua.create_function(process::run)?)?;
    api.set("process", processes)?;
    api.set("state", state_table(lua, owner)?)?;
    api.set("fs", fs_table(lua)?)?;
    api.set("time", time_table(lua)?)?;
    api.set("image", image_table(lua)?)?;
    api.set("dbus", dbus_table(lua, owner, events.clone(), services)?)?;
    api.set("uri", uri_table(lua)?)?;
    let overlay = lua.create_table()?;
    for operation in ["open", "close", "toggle"] {
        let sender = events.clone();
        let value = operation.to_owned();
        overlay.set(
            operation,
            lua.create_function(move |_, ()| {
                sender
                    .send(Event::Overlay(value.clone()))
                    .map_err(mlua::Error::external)
            })?,
        )?;
    }
    api.set("overlay", overlay)?;
    api.set(
        "log",
        lua.create_function(|_, (level, message): (String, String)| {
            eprintln!("gisland [{level}]: {message}");
            Ok(())
        })?,
    )?;
    let environment = lua.create_table()?;
    environment.set("asset_root", asset_root.to_string_lossy().as_ref())?;
    environment.set(
        "config_home",
        config_home().map(|path| path.to_string_lossy().into_owned()),
    )?;
    api.set("environment", environment)?;
    Ok(api)
}

fn add_timer(
    lua: &Lua,
    timers: &Arc<Mutex<Vec<Timer>>>,
    owner: &str,
    milliseconds: u64,
    callback: Function,
) -> mlua::Result<u64> {
    let id = NEXT_TIMER.fetch_add(1, Ordering::Relaxed);
    timers.lock().unwrap().push(Timer {
        id,
        owner: owner.to_owned(),
        due: Instant::now() + Duration::from_millis(milliseconds),
        callback: lua.create_registry_value(callback)?,
    });
    Ok(id)
}

fn state_root(owner: &str) -> mlua::Result<PathBuf> {
    if owner.is_empty()
        || !owner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(mlua::Error::external("invalid module owner"));
    }
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or_else(|| mlua::Error::external("state home is unavailable"))?;
    Ok(base.join("gisland").join(owner))
}
fn state_file(owner: &str, name: &str) -> mlua::Result<PathBuf> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(mlua::Error::external("invalid state name"));
    }
    Ok(state_root(owner)?.join(format!("{name}.json")))
}
fn state_table(lua: &Lua, owner: &str) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let load_owner = owner.to_owned();
    table.set(
        "load",
        lua.create_function(move |lua, (name, default): (String, Value)| {
            let path = state_file(&load_owner, &name)?;
            if !path.is_file() {
                return Ok(default);
            }
            let bytes = fs::read(path).map_err(mlua::Error::external)?;
            if bytes.len() > 1_048_576 {
                return Err(mlua::Error::external("state exceeds 1 MiB"));
            }
            let json: Json = serde_json::from_slice(&bytes).map_err(mlua::Error::external)?;
            lua.to_value(&json)
        })?,
    )?;
    let save_owner = owner.to_owned();
    table.set(
        "save",
        lua.create_function(move |lua, (name, value): (String, Value)| {
            let path = state_file(&save_owner, &name)?;
            let json: Json = lua.from_value(value)?;
            let bytes = serde_json::to_vec(&json).map_err(mlua::Error::external)?;
            if bytes.len() > 1_048_576 {
                return Err(mlua::Error::external("state exceeds 1 MiB"));
            }
            fs::create_dir_all(path.parent().unwrap()).map_err(mlua::Error::external)?;
            let temporary = path.with_extension("tmp");
            let mut options = fs::OpenOptions::new();
            options.create(true).truncate(true).write(true).mode(0o600);
            let mut file = options.open(&temporary).map_err(mlua::Error::external)?;
            use std::io::Write;
            file.write_all(&bytes).map_err(mlua::Error::external)?;
            file.sync_all().map_err(mlua::Error::external)?;
            fs::rename(temporary, path).map_err(mlua::Error::external)
        })?,
    )?;
    Ok(table)
}

fn fs_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "read_regular",
        lua.create_function(|_, (path, maximum): (String, Option<u64>)| {
            let metadata = fs::symlink_metadata(&path).map_err(mlua::Error::external)?;
            let maximum = maximum.unwrap_or(1_048_576).min(1_048_576);
            if !metadata.file_type().is_file() || metadata.len() > maximum {
                return Err(mlua::Error::external("file is not a bounded regular file"));
            }
            fs::read(path).map_err(mlua::Error::external)
        })?,
    )?;
    Ok(table)
}

fn time_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "now",
        lua.create_function(|lua, options: Option<Table>| {
            let timezone = options
                .as_ref()
                .and_then(|value| value.get::<String>("timezone").ok())
                .unwrap_or_default();
            let locale = options
                .as_ref()
                .and_then(|value| value.get::<String>("locale").ok())
                .or_else(|| env::var("LC_TIME").ok())
                .or_else(|| env::var("LANG").ok())
                .unwrap_or_default();
            date_table(lua, Utc::now().timestamp_millis(), &timezone, &locale)
        })?,
    )?;
    table.set(
        "format",
        lua.create_function(|_, (value, format): (Table, String)| {
            let source = value.get::<Table>("source").ok();
            let timezone = value
                .get::<String>("_timezone")
                .or_else(|_| source.as_ref().unwrap().get("_timezone"))?;
            let locale_name = value
                .get::<String>("_locale")
                .or_else(|_| source.as_ref().unwrap().get("_locale"))
                .unwrap_or_default();
            let timestamp = value
                .get::<i64>("_timestamp")
                .or_else(|_| source.as_ref().unwrap().get("_timestamp"))?;
            let fields = (
                value.get::<i32>("year").ok(),
                value.get::<u32>("month").ok(),
                value.get::<u32>("day").ok(),
            );
            let locale = locale(&locale_name);
            if timezone.is_empty() {
                let mut date = Local
                    .timestamp_millis_opt(timestamp)
                    .single()
                    .ok_or_else(|| mlua::Error::external("invalid time"))?;
                if let (Some(year), Some(month), Some(day)) = fields {
                    date = Local
                        .with_ymd_and_hms(
                            year,
                            month,
                            day,
                            date.hour(),
                            date.minute(),
                            date.second(),
                        )
                        .single()
                        .ok_or_else(|| mlua::Error::external("invalid date"))?;
                }
                Ok(date.format_localized(&format, locale).to_string())
            } else {
                let tz: Tz = timezone.parse().map_err(mlua::Error::external)?;
                let mut date = tz
                    .timestamp_millis_opt(timestamp)
                    .single()
                    .ok_or_else(|| mlua::Error::external("invalid time"))?;
                if let (Some(year), Some(month), Some(day)) = fields {
                    date = tz
                        .with_ymd_and_hms(
                            year,
                            month,
                            day,
                            date.hour(),
                            date.minute(),
                            date.second(),
                        )
                        .single()
                        .ok_or_else(|| mlua::Error::external("invalid date"))?;
                }
                Ok(date.format_localized(&format, locale).to_string())
            }
        })?,
    )?;
    Ok(table)
}

fn locale(value: &str) -> Locale {
    match value.split(['.', '_', '-']).next().unwrap_or("") {
        "fr" => Locale::fr_FR,
        "de" => Locale::de_DE,
        "es" => Locale::es_ES,
        "it" => Locale::it_IT,
        "pt" => Locale::pt_PT,
        "en" => Locale::en_GB,
        _ => Locale::en_US,
    }
}

fn date_table(lua: &Lua, timestamp: i64, timezone: &str, locale: &str) -> mlua::Result<Table> {
    let (year, month, day, hour, minute, second, millisecond) = if timezone.is_empty() {
        let value = Local
            .timestamp_millis_opt(timestamp)
            .single()
            .ok_or_else(|| mlua::Error::external("invalid current time"))?;
        (
            value.year(),
            value.month(),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.timestamp_subsec_millis(),
        )
    } else {
        let tz: Tz = timezone.parse().map_err(mlua::Error::external)?;
        let value = tz
            .timestamp_millis_opt(timestamp)
            .single()
            .ok_or_else(|| mlua::Error::external("invalid current time"))?;
        (
            value.year(),
            value.month(),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.timestamp_subsec_millis(),
        )
    };
    let table = lua.create_table()?;
    table.set("year", year)?;
    table.set("month", month)?;
    table.set("day", day)?;
    table.set("hour", hour)?;
    table.set("minute", minute)?;
    table.set("second", second)?;
    table.set("millisecond", millisecond)?;
    table.set("_timestamp", timestamp)?;
    table.set("_timezone", timezone)?;
    table.set("_locale", locale)?;
    Ok(table)
}

fn image_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    for (name, kind) in [("load", "path"), ("icon", "icon")] {
        table.set(
            name,
            lua.create_function(move |lua, source: String| {
                if source.len() > 4096 || source.as_bytes().contains(&0) {
                    return Err(mlua::Error::external("invalid image source"));
                }
                let value = lua.create_table()?;
                value.set("kind", kind)?;
                value.set("source", source)?;
                Ok(value)
            })?,
        )?;
    }
    table.set(
        "rgba",
        lua.create_function(|lua, data: Table| {
            let value = lua.create_table()?;
            value.set("kind", "rgba")?;
            value.set("data", data)?;
            Ok(value)
        })?,
    )?;
    let rgba: Function = table.get("rgba")?;
    table.set("from_raw", rgba)?;
    table.set(
        "resource",
        lua.create_function(|lua, (id, image): (String, Value)| {
            let resource = lua.create_table()?;
            resource.set("id", id)?;
            resource.set("image", image)?;
            Ok(resource)
        })?,
    )?;
    Ok(table)
}
static NEXT_SERVICE: AtomicU64 = AtomicU64::new(1);

fn dbus_table(
    lua: &Lua,
    owner: &str,
    events: mpsc::Sender<Event>,
    services: Arc<Mutex<HashMap<(u64, String), ServiceCallback>>>,
) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "proxy",
        lua.create_function(|lua, spec: Table| {
            let bus: String = spec.get("bus")?;
            let name: String = spec.get("name")?;
            let path: String = spec.get("path")?;
            let interface: String = spec.get("interface")?;
            let connection = if bus == "system" {
                zbus::blocking::Connection::system()
            } else {
                zbus::blocking::Connection::session()
            }
            .map_err(mlua::Error::external)?;
            let proxy = lua.create_table()?;
            proxy.set(
                "get",
                lua.create_function(move |lua, (_self, property): (Table, String)| {
                    let properties = zbus::blocking::Proxy::new(
                        &connection,
                        name.as_str(),
                        path.as_str(),
                        "org.freedesktop.DBus.Properties",
                    )
                    .map_err(mlua::Error::external)?;
                    let value: zbus::zvariant::OwnedValue = properties
                        .call("Get", &(interface.as_str(), property.as_str()))
                        .map_err(mlua::Error::external)?;
                    lua.to_value(&owned_json(value))
                })?,
            )?;
            proxy.set(
                "watch",
                lua.create_function(|_, (_self, _callback): (Table, Function)| Ok(false))?,
            )?;
            Ok(proxy)
        })?,
    )?;

    let owner = owner.to_owned();
    table.set(
        "service",
        lua.create_function(move |lua, spec: Table| {
            let bus: String = spec.get("bus")?;
            let name: String = spec.get("name")?;
            let path: String = spec.get("path")?;
            let interface: String = spec.get("interface")?;
            let methods: Table = spec.get("methods")?;
            let id = NEXT_SERVICE.fetch_add(1, Ordering::Relaxed);
            for pair in methods.pairs::<String, Function>() {
                let (method, callback) = pair?;
                services.lock().unwrap().insert(
                    (id, method),
                    ServiceCallback {
                        owner: owner.clone(),
                        callback: lua.create_registry_value(callback)?,
                    },
                );
            }
            let connection = if bus == "system" {
                zbus::blocking::Connection::system()
            } else {
                zbus::blocking::Connection::session()
            }
            .map_err(mlua::Error::external)?;
            connection
                .request_name_with_flags(name.as_str(), Default::default())
                .map_err(mlua::Error::external)?;
            let listener = connection.clone();
            let listener_events = events.clone();
            let listener_path = path.clone();
            let listener_interface = interface.clone();
            let listener_name = name.clone();
            thread::spawn(move || {
                listen_dbus(
                    listener,
                    id,
                    listener_path,
                    listener_interface,
                    listener_name,
                    listener_events,
                )
            });
            let service = lua.create_table()?;
            let signal_connection = connection.clone();
            let signal_path = path.clone();
            let signal_interface = interface.clone();
            service.set(
                "signal",
                lua.create_function(move |_, values: MultiValue| {
                    let mut values = values.into_vec().into_iter();
                    let _self = values.next();
                    let signal = match values.next() {
                        Some(Value::String(value)) => value.to_str()?.to_owned(),
                        _ => return Err(mlua::Error::external("signal name is required")),
                    };
                    let id = match values.next() {
                        Some(Value::Integer(value)) => value as u32,
                        Some(Value::Number(value)) => value as u32,
                        _ => 0,
                    };
                    if signal == "NotificationClosed" {
                        let reason = match values.next() {
                            Some(Value::Integer(value)) => value as u32,
                            Some(Value::Number(value)) => value as u32,
                            _ => 0,
                        };
                        signal_connection
                            .emit_signal(
                                None::<()>,
                                signal_path.as_str(),
                                signal_interface.as_str(),
                                signal.as_str(),
                                &(id, reason),
                            )
                            .map_err(mlua::Error::external)?;
                    } else if signal == "ActionInvoked" {
                        let action = match values.next() {
                            Some(Value::String(value)) => value.to_str()?.to_owned(),
                            _ => String::new(),
                        };
                        signal_connection
                            .emit_signal(
                                None::<()>,
                                signal_path.as_str(),
                                signal_interface.as_str(),
                                signal.as_str(),
                                &(id, action),
                            )
                            .map_err(mlua::Error::external)?;
                    }
                    Ok(())
                })?,
            )?;
            let close_connection = connection.clone();
            let close_name = name.clone();
            service.set(
                "close",
                lua.create_function(move |_, _self: Table| {
                    close_connection
                        .release_name(close_name.as_str())
                        .map_err(mlua::Error::external)?;
                    Ok(())
                })?,
            )?;
            Ok(service)
        })?,
    )?;
    Ok(table)
}

fn listen_dbus(
    connection: zbus::blocking::Connection,
    service: u64,
    path: String,
    interface: String,
    bus_name: String,
    events: mpsc::Sender<Event>,
) {
    for message in zbus::blocking::MessageIterator::from(&connection).flatten() {
        let header = message.header();
        if header.interface().map(|value| value.as_str()) == Some("org.freedesktop.DBus")
            && header.member().map(|value| value.as_str()) == Some("NameLost")
            && message.body().deserialize::<String>().ok().as_deref() == Some(bus_name.as_str())
        {
            break;
        }
        if header.path().map(|value| value.as_str()) != Some(path.as_str())
            || header.interface().map(|value| value.as_str()) != Some(interface.as_str())
        {
            continue;
        }
        let Some(method) = header.member().map(|value| value.as_str().to_owned()) else {
            continue;
        };
        let arguments = match decode_dbus_call(&message, &method) {
            Ok(value) => value,
            Err(error) => {
                let _ = connection.reply_error(
                    &header,
                    "org.freedesktop.DBus.Error.InvalidArgs",
                    &error.to_string(),
                );
                continue;
            }
        };
        let (reply, receiver) = mpsc::channel();
        if events
            .send(Event::Dbus(DbusCall {
                service,
                method: method.clone(),
                arguments,
                reply,
            }))
            .is_err()
        {
            break;
        }
        match receiver.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(value)) => {
                if let Err(error) = reply_dbus(&connection, &header, &method, value) {
                    eprintln!("gisland: D-Bus reply failed: {error}");
                }
            }
            Ok(Err(error)) => {
                let _ =
                    connection.reply_error(&header, "org.freedesktop.DBus.Error.Failed", &error);
            }
            Err(error) => {
                let _ = connection.reply_error(
                    &header,
                    "org.freedesktop.DBus.Error.Timeout",
                    &error.to_string(),
                );
            }
        }
    }
}
type NotificationArguments = (
    String,
    u32,
    String,
    String,
    String,
    Vec<String>,
    HashMap<String, zbus::zvariant::OwnedValue>,
    i32,
);

fn decode_dbus_call(message: &zbus::Message, method: &str) -> zbus::Result<Json> {
    Ok(match method {
        "Notify" => {
            let (app, replaces, icon, summary, body, actions, hints, timeout): NotificationArguments = message.body().deserialize()?;
            let hints = hints
                .into_iter()
                .map(|(key, value)| (key, owned_json(value)))
                .collect();
            Json::Array(vec![
                app.into(),
                replaces.into(),
                icon.into(),
                summary.into(),
                body.into(),
                serde_json::to_value(actions).unwrap(),
                Json::Object(hints),
                timeout.into(),
            ])
        }
        "CloseNotification" => {
            let id: u32 = message.body().deserialize()?;
            Json::Array(vec![id.into()])
        }
        _ => Json::Array(Vec::new()),
    })
}

fn reply_dbus(
    connection: &zbus::blocking::Connection,
    header: &zbus::message::Header<'_>,
    method: &str,
    value: Json,
) -> zbus::Result<()> {
    match method {
        "GetCapabilities" => connection.reply(
            header,
            &serde_json::from_value::<Vec<String>>(value).unwrap_or_default(),
        ),
        "GetServerInformation" => {
            let values = serde_json::from_value::<Vec<String>>(value).unwrap_or_default();
            connection.reply(
                header,
                &(
                    values.first().cloned().unwrap_or_default(),
                    values.get(1).cloned().unwrap_or_default(),
                    values.get(2).cloned().unwrap_or_default(),
                    values.get(3).cloned().unwrap_or_default(),
                ),
            )
        }
        "Notify" => connection.reply(header, &(value.as_u64().unwrap_or(0) as u32)),
        _ => connection.reply(header, &()),
    }
}

fn owned_json(value: zbus::zvariant::OwnedValue) -> Json {
    if let Ok(value) = serde_json::to_value(&value) {
        return value;
    }
    if let Ok(value) = bool::try_from(value.try_clone().unwrap()) {
        return value.into();
    }
    if let Ok(value) = f64::try_from(value.try_clone().unwrap()) {
        return serde_json::Number::from_f64(value)
            .map(Json::Number)
            .unwrap_or(Json::Null);
    }
    if let Ok(value) = u64::try_from(value.try_clone().unwrap()) {
        return value.into();
    }
    if let Ok(value) = i64::try_from(value.try_clone().unwrap()) {
        return value.into();
    }
    if let Ok(value) = String::try_from(value) {
        return value.into();
    }
    Json::Null
}
fn uri_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "open",
        lua.create_function(|_, uri: String| {
            if !["http://", "https://", "mailto:"]
                .iter()
                .any(|prefix| uri.starts_with(prefix))
            {
                return Err(mlua::Error::external("URI scheme is not allowed"));
            }
            std::process::Command::new("xdg-open")
                .arg(uri)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(mlua::Error::external)?;
            Ok(true)
        })?,
    )?;
    Ok(table)
}
