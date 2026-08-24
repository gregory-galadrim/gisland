use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::OpenOptionsExt,
        unix::net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
};

pub const MAX_REQUEST: usize = 65_536;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}
pub struct Envelope {
    pub request: Request,
    pub reply: mpsc::Sender<Response>,
}
pub struct InstanceLock {
    file: fs::File,
}
impl Drop for InstanceLock {
    fn drop(&mut self) {
        /* SAFETY: valid owned fd. */
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub fn runtime_path(name: &str) -> Result<PathBuf> {
    let directory = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    let metadata = fs::metadata(&directory)
        .with_context(|| format!("missing runtime directory {}", directory.display()))?;
    if !metadata.is_dir() {
        bail!("runtime path is not a directory: {}", directory.display());
    }
    Ok(directory.join(name))
}

pub fn acquire_lock() -> Result<InstanceLock> {
    let path = runtime_path("gisland.lock")?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("gisland is already running");
    }
    Ok(InstanceLock { file })
}

pub fn start_server(sender: mpsc::Sender<Envelope>) -> Result<()> {
    let path = runtime_path("gisland.sock")?;
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("cannot remove stale {}", path.display()))?;
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("cannot bind {}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(&path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&path, permissions)?;
    thread::Builder::new()
        .name("gisland-ipc".into())
        .spawn(move || {
            for connection in listener.incoming() {
                match connection {
                    Ok(stream) => {
                        let sender = sender.clone();
                        thread::spawn(move || {
                            if let Err(error) = handle_client(stream, sender) {
                                eprintln!("gisland: IPC error: {error:#}");
                            }
                        });
                    }
                    Err(error) => eprintln!("gisland: cannot accept IPC connection: {error}"),
                }
            }
        })?;
    Ok(())
}

fn handle_client(mut stream: UnixStream, sender: mpsc::Sender<Envelope>) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    ensure_same_user(&stream)?;
    let mut line = Vec::new();
    BufReader::new(stream.try_clone()?)
        .take((MAX_REQUEST + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if line.len() > MAX_REQUEST || !line.ends_with(b"\n") {
        bail!("request is too large or incomplete");
    }
    let request: Request = serde_json::from_slice(&line).context("invalid request JSON")?;
    let (reply, receiver) = mpsc::channel();
    sender
        .send(Envelope { request, reply })
        .context("application is stopping")?;
    let response = receiver
        .recv_timeout(Duration::from_secs(10))
        .context("request timed out")?;
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn ensure_same_user(stream: &UnixStream) -> Result<()> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot inspect peer credentials");
    }
    if credentials.uid != unsafe { libc::getuid() } {
        bail!("peer user does not own gisland");
    }
    Ok(())
}

pub fn client(request: &Request) -> Result<Response> {
    let path = runtime_path("gisland.sock")?;
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("cannot connect to {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).context("invalid response from gisland")
}

pub fn run_cli(arguments: &[String]) -> Result<i32> {
    if arguments.is_empty() || matches!(arguments[0].as_str(), "help" | "--help") {
        println!(
            "usage: gislandctl <open|close|toggle|status|modules|reload|module restart|activate|activate-open|dismiss|action> [arguments]"
        );
        return Ok(0);
    }
    let request = parse_cli(arguments)?;
    let response = client(&request)?;
    if let Some(data) = response.data {
        if request.command == "status" && request.args.iter().any(|value| value == "--json") {
            println!("{}", serde_json::to_string_pretty(&data)?);
        } else if let Some(text) = data.as_str() {
            println!("{text}");
        } else {
            println!("{}", serde_json::to_string_pretty(&data)?);
        }
    }
    if let Some(error) = response.error {
        eprintln!("gislandctl: {error}");
    }
    Ok(if response.ok { 0 } else { 1 })
}

fn parse_cli(arguments: &[String]) -> Result<Request> {
    let (command, args) = if arguments[0] == "module" {
        if arguments.get(1).map(String::as_str) != Some("restart") || arguments.len() != 3 {
            bail!("usage: gislandctl module restart <instance>");
        }
        ("module-restart".to_owned(), vec![arguments[2].clone()])
    } else {
        (arguments[0].clone(), arguments[1..].to_vec())
    };
    let allowed = [
        "open",
        "close",
        "toggle",
        "status",
        "modules",
        "reload",
        "activate",
        "activate-open",
        "dismiss",
        "action",
    ];
    if command != "module-restart" && !allowed.contains(&command.as_str()) {
        bail!("unknown command: {command}");
    }
    Ok(Request { command, args })
}

pub fn ok(data: Option<Value>) -> Response {
    Response {
        ok: true,
        error: None,
        data,
    }
}
pub fn failure(error: impl ToString) -> Response {
    Response {
        ok: false,
        error: Some(error.to_string()),
        data: None,
    }
}
pub fn basic_status(visibility: &str, lua: Value) -> Value {
    let mut value = json!({ "format_version": 2, "pid": std::process::id(), "visibility": visibility, "socket": runtime_path("gisland.sock").ok() });
    if let (Some(target), Some(source)) = (value.as_object_mut(), lua.as_object()) {
        target.extend(source.clone());
    }
    value
}
