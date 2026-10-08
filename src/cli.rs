use std::ffi::OsString;
use std::path::PathBuf;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Args, CommandFactory, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "molso",
    version,
    about = "Store credentials locally and pass them directly to a command",
    after_help = "GET STARTED:\n  molso init\n  molso put github/agent                 # hidden input in your terminal\n  molso exec --env GH_TOKEN=github/agent -- gh auth status\n\nAGENT USE:\n  Use exec to pass a credential to a child without putting its value in arguments.\n  Use get only with a consumer in the same shell invocation.\n  Run molso help <command> for examples."
)]
pub struct Options {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Vault path (overrides MOLSO_VAULT and the platform data directory)"
    )]
    pub vault: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Key file path (overrides MOLSO_KEY_FILE and the platform data directory)"
    )]
    pub key_file: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(
        about = "Create a key file and an empty vault",
        after_help = "Existing files are never overwritten. A missing vault can be recreated with\nits existing 32-byte key.\n\nNEXT: Run molso put github/agent in your terminal to add a credential."
    )]
    Init,
    #[command(
        about = "Add a credential using hidden input or a raw stdin stream",
        after_help = "EXAMPLES:\n  molso put github/agent                 # hidden terminal input; Enter ends it\n  molso put github/agent --update        # explicitly replace an existing value\n  credential-source | molso put github/agent --stdin\n\nEmpty input is rejected without saving. To intentionally import an empty\nvalue, combine --stdin with --allow-empty. Non-empty stdin preserves every\nbyte, including a trailing newline. Use -- before a name beginning with '-'.\nCredential values cannot be passed as arguments."
    )]
    Put {
        #[arg(value_name = "SERVICE/ACCOUNT", value_parser = parse_name)]
        name: String,
        #[arg(
            long,
            help = "Replace an existing entry; fails if the entry is missing"
        )]
        update: bool,
        #[arg(
            long = "stdin",
            help = "Read exact bytes from stdin instead of hidden terminal input"
        )]
        from_stdin: bool,
        #[arg(
            long,
            requires = "from_stdin",
            help = "Allow an empty stdin stream to create or replace a credential"
        )]
        allow_empty: bool,
    },
    #[command(about = "List stored names, sorted, one per line")]
    List,
    #[command(
        about = "Write raw credential bytes to a pipe (refuses a terminal)",
        after_help = "Use get and its consumer in the same shell invocation:\n  molso get github/agent | consumer\n\nFor direct delivery without a shell pipeline, prefer:\n  molso exec --stdin github/agent -- consumer\n\nOutput has no added newline. Never call get alone from an agent tool runner."
    )]
    Get {
        #[arg(value_name = "SERVICE/ACCOUNT", value_parser = parse_name)]
        name: String,
    },
    #[command(about = "Delete a stored credential; fails if the entry is missing")]
    Remove {
        #[arg(value_name = "SERVICE/ACCOUNT", value_parser = parse_name)]
        name: String,
    },
    #[command(
        about = "Run a command with credentials in its environment or stdin",
        after_help = "EXAMPLES:\n  molso exec --env GH_TOKEN=github/agent -- gh auth status\n  molso exec --stdin github/agent -- consumer\n\nUse --env when the command expects an environment variable; use --stdin when\nit reads the credential from standard input. --stdin replaces the child's\ninput and closes it after delivery. Arguments after -- belong to the child,\nincluding its --help. Its stdout/stderr are forwarded; choose a consumer that\ndoes not print credentials. Child exit codes are passed through; wrapper\nfailures use 125/126/127. No shell is started automatically."
    )]
    Exec(Exec),
}

#[derive(Debug, Args)]
pub struct Exec {
    #[arg(long, value_name = "VAR=SERVICE/ACCOUNT", value_parser = parse_env, help = "Set a credential in the child environment; repeat for multiple variables")]
    pub env: Vec<(String, String)>,
    #[arg(long, value_name = "SERVICE/ACCOUNT", value_parser = parse_name, help = "Write one credential to child stdin, then close it")]
    pub stdin: Option<String>,
    #[arg(last = true, required = true, num_args = 1.., value_name = "COMMAND", allow_hyphen_values = true, help = "Executable and arguments to run; everything after -- belongs to the child")]
    pub command: Vec<OsString>,
}

pub fn parse<I>(args: I) -> Result<Options, clap::Error>
where
    I: IntoIterator<Item = OsString>,
{
    Options::try_parse_from(std::iter::once(OsString::from("molso")).chain(args))
        .map_err(|mut error| {
            // An extra put argument may be a mistakenly supplied credential; do not echo it.
            let is_put = error.get(ContextKind::Usage)
                .is_some_and(|usage| usage.to_string().starts_with("Usage: molso put "));
            if is_put && matches!(error.kind(), ErrorKind::UnknownArgument | ErrorKind::TooManyValues) {
                let mut message = "unexpected put argument. Supply one SERVICE/ACCOUNT and enter the credential through hidden input or --stdin.".to_string();
                // SuggestedArg comes from declared option names; other contexts may echo input.
                if let Some(ContextValue::String(option)) = error.get(ContextKind::SuggestedArg) {
                    message.push_str(&format!("\n\nDid you mean '{option}'?"));
                }
                message.push_str("\n\nFor usage, run molso help put.\n");
                clap::Error::raw(error.kind(), message)
            } else if error.kind() == ErrorKind::UnknownArgument
                && error.get(ContextKind::Usage)
                    .is_some_and(|usage| usage.to_string().starts_with("Usage: molso exec "))
                && matches!(error.get(ContextKind::InvalidArg), Some(ContextValue::String(argument)) if !argument.starts_with('-'))
            {
                error.insert(ContextKind::Suggested, ContextValue::StyledStrs(vec![
                    "place -- before the command and its arguments, for example: molso exec --stdin SERVICE/ACCOUNT -- consumer".into(),
                ]));
                error
            } else {
                error
            }
        })
}

pub fn print_help() -> std::io::Result<()> {
    Options::command().print_help()?;
    println!();
    Ok(())
}

fn parse_name(name: &str) -> Result<String, String> {
    validate_name(name)?;
    Ok(name.to_string())
}

pub fn validate_name(name: &str) -> Result<(), String> {
    let Some((service, account)) = name.split_once('/') else {
        return Err("expected SERVICE/ACCOUNT, for example github/agent".to_string());
    };
    if service.is_empty() || account.is_empty() || account.contains('/') {
        return Err("expected two non-empty segments: SERVICE/ACCOUNT".to_string());
    }
    if name.chars().any(char::is_control) {
        return Err("control characters are not allowed in credential names".to_string());
    }
    Ok(())
}

fn parse_env(mapping: &str) -> Result<(String, String), String> {
    let (var, reference) = mapping
        .split_once('=')
        .ok_or("expected VAR=SERVICE/ACCOUNT, for example GH_TOKEN=github/agent")?;
    if var.is_empty() || var.contains('\0') {
        return Err("environment variable name must be non-empty and contain no NUL".to_string());
    }
    validate_name(reference)?;
    Ok((var.to_string(), reference.to_string()))
}
