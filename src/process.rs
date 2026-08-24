use anyhow::{Context, Result};
use mlua::{Lua, Table};
use std::{
    io::Read,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    thread,
    time::Duration,
};
use wait_timeout::ChildExt;

pub fn run(lua: &Lua, (argv, options): (Table, Option<Table>)) -> mlua::Result<Table> {
    let args = argv
        .sequence_values::<String>()
        .collect::<mlua::Result<Vec<_>>>()?;
    if args.is_empty() {
        return Err(mlua::Error::external("process argv cannot be empty"));
    }
    let timeout_ms = options
        .as_ref()
        .and_then(|value| value.get::<u64>("timeout_ms").ok())
        .unwrap_or(5_000)
        .clamp(1, 60_000);
    let capture_bytes = options
        .as_ref()
        .and_then(|value| value.get::<u64>("capture_bytes").ok())
        .unwrap_or(65_536)
        .clamp(1, 1_048_576) as usize;
    let result = execute(&args, timeout_ms, capture_bytes).map_err(mlua::Error::external)?;
    let table = lua.create_table()?;
    table.set("ok", result.status == 0 && !result.timed_out)?;
    table.set("status", result.status)?;
    table.set("timed_out", result.timed_out)?;
    table.set("output", result.output)?;
    Ok(table)
}

struct Output {
    status: i32,
    timed_out: bool,
    output: String,
}

fn execute(argv: &[String], timeout_ms: u64, capture_bytes: usize) -> Result<Output> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe and does not access parent memory.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot run {}", argv[0]))?;
    let mut stdout = child.stdout.take().context("missing child stdout")?;
    let mut stderr = child.stderr.take().context("missing child stderr")?;
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout
            .by_ref()
            .take((capture_bytes + 1) as u64)
            .read_to_end(&mut bytes);
        bytes.truncate(capture_bytes);
        bytes
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr
            .by_ref()
            .take((capture_bytes + 1) as u64)
            .read_to_end(&mut bytes);
        bytes.truncate(capture_bytes);
        bytes
    });
    let status = child
        .wait_timeout(Duration::from_millis(timeout_ms))
        .context("cannot wait for child")?;
    let timed_out = status.is_none();
    if timed_out {
        /* SAFETY: child created a new session with id equal to its pid. */
        unsafe {
            libc::killpg(child.id() as i32, libc::SIGKILL);
        }
    }
    let status = match status {
        Some(status) => status,
        None => child.wait().context("cannot reap child")?,
    };
    let mut bytes = out.join().unwrap_or_default();
    bytes.extend(err.join().unwrap_or_default());
    bytes.truncate(capture_bytes);
    Ok(Output {
        status: status.code().unwrap_or(128),
        timed_out,
        output: String::from_utf8_lossy(&bytes).into_owned(),
    })
}
