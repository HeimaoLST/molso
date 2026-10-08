use std::io::{self, Write};
use std::process::{Command, ExitStatus, Stdio};

use crate::cli::Exec;
use crate::error::{Error, Result};
use crate::vault::{self, Paths};

#[cfg(windows)]
fn env_names_equal(a: &str, b: &str) -> io::Result<bool> {
    crate::windows::env_names_equal(a, b)
}

#[cfg(not(windows))]
fn env_names_equal(a: &str, b: &str) -> io::Result<bool> {
    Ok(a == b)
}

fn spawn_error(error: io::Error) -> Error {
    let (code, action) = match error.kind() {
        io::ErrorKind::NotFound => (127, "command not found; check its name or PATH"),
        io::ErrorKind::PermissionDenied => {
            (126, "command is not executable; check execute permissions")
        }
        _ => (125, "failed to start command"),
    };
    Error::wrapper(code, format!("{action}: {error}"))
}

fn status_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        125
    }
}

pub fn exec(paths: &Paths, exec: Exec) -> Result<i32> {
    // Duplicate variable mappings are rejected per platform rules before any
    // child is started, so a later mapping can never silently overwrite one.
    let mut seen: Vec<&str> = Vec::new();
    for (var, _) in &exec.env {
        for existing in &seen {
            let equal = env_names_equal(existing, var).map_err(|e| {
                Error::wrapper(
                    125,
                    format!("failed to compare environment variable names: {e}"),
                )
            })?;
            if equal {
                return Err(Error::wrapper(
                    125,
                    format!("duplicate environment variable mapping {var:?}"),
                ));
            }
        }
        seen.push(var);
    }

    // Fetch every referenced credential under a single shared lock, then
    // release the lock before spawning or waiting. Any lookup/decrypt failure
    // is a wrapper failure, not a molso usage error.
    let mut names: Vec<String> = exec.env.iter().map(|(_, name)| name.clone()).collect();
    if let Some(name) = &exec.stdin {
        names.push(name.clone());
    }
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut values = if refs.is_empty() {
        Vec::new()
    } else {
        vault::get_many(paths, &refs).map_err(|e| Error::wrapper(125, e.message))?
    };
    let stdin_value = if exec.stdin.is_some() {
        values.pop()
    } else {
        None
    };

    // Validate all --env values before spawning anything.
    for (index, (var, _)) in exec.env.iter().enumerate() {
        let value = &values[index];
        let text = std::str::from_utf8(value).map_err(|_| {
            Error::wrapper(
                125,
                format!("credential for {var} is not valid UTF-8; use --stdin with a consumer that accepts raw bytes"),
            )
        })?;
        if text.contains('\0') {
            return Err(Error::wrapper(
                125,
                format!(
                    "credential for {var} contains NUL; use --stdin with a consumer that accepts raw bytes"
                ),
            ));
        }
    }

    let program = &exec.command[0];
    let mut command = Command::new(program);
    command.args(&exec.command[1..]);
    for (index, (var, _)) in exec.env.iter().enumerate() {
        let text = std::str::from_utf8(&values[index]).expect("validated above");
        command.env(var, text);
    }
    command.stdin(if stdin_value.is_some() {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });
    command.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Err(spawn_error(error)),
    };
    // Release molso's own command and environment copies immediately; the
    // child already holds what it needs in the OS.
    drop(command);
    drop(values);

    let mut delivery_failed = false;
    if let Some(value) = stdin_value
        && let Some(mut stdin) = child.stdin.take()
        && stdin.write_all(&value).is_err()
    {
        delivery_failed = true;
    }
    // `value` is dropped (and zeroized) here, before the child wait.

    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            // `Child` does not kill or reap on drop; clean up explicitly.
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::wrapper(
                125,
                format!("failed to wait for child: {error}"),
            ));
        }
    };

    let code = status_code(status);
    if delivery_failed && code == 0 {
        eprintln!("molso: credential delivery to child stdin failed (broken pipe or I/O error)");
        return Ok(125);
    }
    Ok(code)
}
