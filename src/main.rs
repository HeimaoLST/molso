mod cli;
mod error;
mod process;
mod vault;
#[cfg(windows)]
mod windows;

use std::io::{IsTerminal, Read, Write};

use zeroize::Zeroizing;

use crate::cli::Command;
use crate::error::{Error, Result};

fn main() {
    // Compute the status first so every sensitive object has been dropped by
    // the time `exit` runs; `std::process::exit` does not run destructors.
    std::process::exit(run());
}

fn run() -> i32 {
    let options = match cli::parse(std::env::args_os().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            let code = error.exit_code();
            return if error.print().is_ok() { code } else { 2 };
        }
    };
    match dispatch(options) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("molso: {}", error.message);
            error.code
        }
    }
}

fn dispatch(options: cli::Options) -> Result<i32> {
    let Some(command) = options.command else {
        cli::print_help()?;
        return Ok(0);
    };

    let paths = vault::Paths::resolve(options.vault, options.key_file)?;
    match command {
        Command::Init => {
            vault::init(&paths)?;
            if std::io::stdout().is_terminal() {
                println!("Key file: {}", paths.key.display());
                println!("Vault:    {}", paths.vault.display());
            } else {
                println!("{}", paths.key.display());
                println!("{}", paths.vault.display());
            }
            eprintln!("molso: initialized; add a credential with molso put SERVICE/ACCOUNT");
            Ok(0)
        }
        Command::Put {
            name,
            update,
            from_stdin,
            allow_empty,
        } => {
            vault::check_put(&paths, &name, update)?;
            let secret = read_secret(&name, from_stdin, allow_empty)?;
            let length = secret.len();
            vault::put(&paths, &name, update, secret)?;
            eprintln!("molso: stored {length} bytes for {name}");
            Ok(0)
        }
        Command::List => {
            let names = vault::list(&paths)?;
            if names.is_empty() && std::io::stdout().is_terminal() {
                eprintln!("molso: no credentials stored; add one with molso put SERVICE/ACCOUNT");
            }
            for name in names {
                println!("{name}");
            }
            Ok(0)
        }
        Command::Get { name } => {
            if std::io::stdout().is_terminal() {
                return Err(Error::failure(
                    "refusing to write a credential to a terminal; pipe the output or use exec",
                ));
            }
            let value = vault::get(&paths, &name)?;
            let mut stdout = std::io::stdout().lock();
            if let Err(error) = stdout.write_all(&value).and_then(|_| stdout.flush()) {
                return Err(Error::failure(format!(
                    "failed to write credential: {error}"
                )));
            }
            Ok(0)
        }
        Command::Remove { name } => {
            vault::remove(&paths, &name)?;
            eprintln!("molso: removed {name}");
            Ok(0)
        }
        Command::Exec(exec) => process::exec(&paths, exec),
    }
}

fn read_secret(name: &str, from_stdin: bool, allow_empty: bool) -> Result<Zeroizing<Vec<u8>>> {
    if from_stdin {
        let mut buffer = Zeroizing::new(Vec::new());
        std::io::stdin()
            .read_to_end(&mut buffer)
            .map_err(|e| Error::failure(format!("failed to read stdin: {e}")))?;
        if buffer.is_empty() && !allow_empty {
            return Err(Error::failure(
                "stdin is empty; nothing saved. Check the credential source, or use --stdin --allow-empty to intentionally store an empty value",
            ));
        }
        Ok(buffer)
    } else {
        if !std::io::stdin().is_terminal() {
            return Err(Error::failure(
                "no terminal available for hidden input; use --stdin",
            ));
        }
        let secret = Zeroizing::new(
            rpassword::prompt_password(format!("Credential for {name} (input hidden): "))
                .map_err(|e| Error::failure(format!("failed to read credential: {e}")))?
                .into_bytes(),
        );
        if secret.is_empty() {
            return Err(Error::failure(
                "empty credential not saved; rerun put and enter a value",
            ));
        }
        Ok(secret)
    }
}
