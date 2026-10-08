use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

mod support;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_molso")
}

struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Temp {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("molso-test-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Temp(path)
    }

    fn vault(&self) -> PathBuf {
        self.0.join("vault")
    }

    fn key(&self) -> PathBuf {
        self.0.join("key")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(temp: &Temp) -> Command {
    let mut command = Command::new(bin());
    command.env("MOLSO_VAULT", temp.vault());
    command.env("MOLSO_KEY_FILE", temp.key());
    command.current_dir(&temp.0);
    command
}

fn run(temp: &Temp, args: &[&str]) -> Output {
    command(temp).args(args).output().unwrap()
}

fn run_stdin(temp: &Temp, args: &[&str], input: &[u8]) -> Output {
    let mut child = command(temp)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

/// Run `molso <prefix> -- <test binary> --ignored --exact --nocapture helper`
/// with the given helper role, writing the helper result to `out`.
fn helper_run(
    temp: &Temp,
    prefix: &[&str],
    role: &str,
    env: &[(&str, &str)],
    stdin: Option<&[u8]>,
    out: &Path,
) -> Output {
    let helper = support::helper_program();
    let mut command = command(temp);
    command.args(prefix);
    command.arg(&helper);
    command.args(support::helper_args());
    command.env("MOLSO_TEST_HELPER", role);
    command.env("MOLSO_TEST_OUT", out);
    for (key, value) in env {
        command.env(key, value);
    }
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn result(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

/// Ignored helper test that turns the integration test binary into a plain
/// consumer process for `molso exec`. See `tests/support/mod.rs`.
#[test]
#[ignore]
fn helper() {
    support::helper_main();
}

fn init(temp: &Temp) {
    let output = run(temp, &["init"]);
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn put(temp: &Temp, name: &str, value: &[u8]) {
    let output = run_stdin(temp, &["put", name, "--stdin"], value);
    assert!(
        output.status.success(),
        "put failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn full_loop_preserves_bytes() {
    let temp = Temp::new("loop");
    init(&temp);

    let secret: &[u8] = b"\x00\xff \xe4\xb8\xad\nspace key\r\n";
    put(&temp, "svc/account", secret);

    let got = run(&temp, &["get", "svc/account"]);
    assert!(got.status.success());
    assert_eq!(got.stdout, secret, "get must preserve raw bytes exactly");
    assert!(
        got.stderr.is_empty(),
        "get must not write to stderr on success"
    );

    let list = run(&temp, &["list"]);
    assert_eq!(String::from_utf8(list.stdout).unwrap(), "svc/account\n");

    let removed = run(&temp, &["remove", "svc/account"]);
    assert!(removed.status.success());
    let list = run(&temp, &["list"]);
    assert_eq!(list.stdout, b"");
    assert!(list.stderr.is_empty(), "piped empty lists must stay silent");
    let missing = run(&temp, &["get", "svc/account"]);
    assert_eq!(missing.status.code(), Some(2));
}

#[test]
fn wrong_key_and_corruption_leave_vault_unchanged() {
    let temp = Temp::new("corrupt");
    init(&temp);
    put(&temp, "svc/account", b"fixture-value");
    let original = fs::read(temp.vault()).unwrap();
    let good_key = fs::read(temp.key()).unwrap();

    fs::write(temp.key(), [7u8; 32]).unwrap();
    let wrong = run(&temp, &["get", "svc/account"]);
    assert_eq!(wrong.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&wrong.stderr).contains("fixture-value"));
    assert_eq!(fs::read(temp.vault()).unwrap(), original);

    fs::write(temp.key(), &good_key).unwrap();
    let mut tampered = original.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    fs::write(temp.vault(), &tampered).unwrap();
    let tamper = run(&temp, &["get", "svc/account"]);
    assert_eq!(tamper.status.code(), Some(2));

    fs::write(temp.vault(), &original[..original.len() - 1]).unwrap();
    let truncated = run(&temp, &["get", "svc/account"]);
    assert_eq!(truncated.status.code(), Some(2));

    let mut unknown = original.clone();
    unknown[0] = b'X';
    fs::write(temp.vault(), &unknown).unwrap();
    let unknown_result = run(&temp, &["get", "svc/account"]);
    assert_eq!(unknown_result.status.code(), Some(2));
}

#[test]
fn init_does_not_overwrite_and_reuses_key() {
    let temp = Temp::new("init");
    init(&temp);
    let key = fs::read(temp.key()).unwrap();
    let vault = fs::read(temp.vault()).unwrap();

    let again = run(&temp, &["init"]);
    assert_eq!(again.status.code(), Some(2), "second init must fail");
    let hint = String::from_utf8_lossy(&again.stderr);
    assert!(hint.contains("molso list") && hint.contains("molso put"));
    assert_eq!(fs::read(temp.key()).unwrap(), key, "key must not change");
    assert_eq!(
        fs::read(temp.vault()).unwrap(),
        vault,
        "vault must not change"
    );

    fs::remove_file(temp.vault()).unwrap();
    let recreate = run(&temp, &["init"]);
    assert!(
        recreate.status.success(),
        "init must recreate a missing vault"
    );
    assert_eq!(
        fs::read(temp.key()).unwrap(),
        key,
        "key must be reused, not regenerated"
    );
    put(&temp, "svc/account", b"after-recreate");
    let got = run(&temp, &["get", "svc/account"]);
    assert_eq!(got.stdout, b"after-recreate");
}

#[test]
fn truncated_key_is_rejected() {
    let temp = Temp::new("truncated-key");
    init(&temp);
    put(&temp, "svc/account", b"value");
    fs::write(temp.key(), [1u8; 31]).unwrap();
    fs::remove_file(temp.vault()).ok();
    let output = run(&temp, &["init"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("32 bytes"),
        "must report the key length problem"
    );
}

#[test]
fn put_update_semantics_and_dash_names() {
    let temp = Temp::new("update");
    init(&temp);
    let create = run_stdin(&temp, &["put", "svc/account", "--stdin"], b"one");
    assert!(create.status.success());
    let duplicate = run_stdin(&temp, &["put", "svc/account", "--stdin"], b"two");
    assert_eq!(duplicate.status.code(), Some(2));
    assert_eq!(run(&temp, &["get", "svc/account"]).stdout, b"one");

    let update = run_stdin(
        &temp,
        &["put", "svc/account", "--update", "--stdin"],
        b"two",
    );
    assert!(update.status.success());
    assert_eq!(run(&temp, &["get", "svc/account"]).stdout, b"two");

    let update_missing = run_stdin(&temp, &["put", "svc/other", "--update", "--stdin"], b"x");
    assert_eq!(update_missing.status.code(), Some(2));

    let dash = run_stdin(&temp, &["put", "--stdin", "--", "-svc/acct"], b"dash");
    assert!(
        dash.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&dash.stderr)
    );
    assert_eq!(run(&temp, &["get", "--", "-svc/acct"]).stdout, b"dash");
    let list = run(&temp, &["list"]);
    assert!(
        String::from_utf8(list.stdout)
            .unwrap()
            .contains("-svc/acct")
    );
}

#[test]
fn help_and_errors_are_reported() {
    let temp = Temp::new("help");
    for args in [
        vec!["--help"],
        vec!["init", "--help"],
        vec!["put", "--help"],
        vec!["list", "--help"],
        vec!["get", "--help"],
        vec!["remove", "--help"],
        vec!["exec", "--help"],
    ] {
        let output = run(&temp, &args);
        assert!(output.status.success(), "{args:?} must exit 0");
        assert!(!output.stdout.is_empty(), "{args:?} must print help");
    }
    let unknown = run(&temp, &["frobnicate"]);
    assert_eq!(unknown.status.code(), Some(2));
    let bad_name = run_stdin(&temp, &["put", "noslash", "--stdin"], b"x");
    assert_eq!(bad_name.status.code(), Some(2));
    let dup_stdin = run(
        &temp,
        &["exec", "--stdin", "a/b", "--stdin", "c/d", "--", "true"],
    );
    assert_eq!(dup_stdin.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&dup_stdin.stderr).contains("--stdin"));
}

#[test]
fn put_rejects_accidental_credential_arguments_without_echoing_them() {
    let temp = Temp::new("credential-argument");
    for value in [
        "fictional-accidental-secret",
        "--fictional-accidental-secret",
    ] {
        let output = run(&temp, &["put", "a/b", value]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains(value));
        assert!(String::from_utf8_lossy(&output.stderr).contains("--stdin"));
    }
    assert!(!temp.key().exists() && !temp.vault().exists());
}

#[test]
fn put_suggests_known_options_without_echoing_mistyped_input() {
    let temp = Temp::new("put-typo");
    for (argument, suggestion) in [
        ("--udpate", "--update"),
        ("--stdni", "--stdin"),
        ("--key-fiel", "--key-file"),
        ("--udpate=fictional-accidental-secret", "--update"),
    ] {
        let output = run(&temp, &["put", "a/b", argument]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!error.contains(argument));
        assert!(!error.contains("fictional-accidental-secret"));
        assert!(
            error.contains(&format!("Did you mean '{suggestion}'?")),
            "{error}"
        );
    }
    assert!(!temp.key().exists() && !temp.vault().exists());
}

#[test]
fn stdin_can_intentionally_store_empty_credentials() {
    let temp = Temp::new("empty-stdin");
    init(&temp);
    let created = run_stdin(&temp, &["put", "svc/new", "--stdin", "--allow-empty"], b"");
    assert!(created.status.success());
    put(&temp, "svc/existing", b"old");
    let updated = run_stdin(
        &temp,
        &[
            "put",
            "svc/existing",
            "--stdin",
            "--update",
            "--allow-empty",
        ],
        b"",
    );
    assert!(updated.status.success());
    for name in ["svc/new", "svc/existing"] {
        let got = run(&temp, &["get", name]);
        assert!(got.status.success());
        assert!(got.stdout.is_empty());
    }
    assert_eq!(run(&temp, &["list"]).stdout, b"svc/existing\nsvc/new\n");
}

#[test]
fn failed_empty_producer_neither_creates_nor_overwrites_credentials() {
    let temp = Temp::new("failed-import");
    init(&temp);
    put(&temp, "svc/existing", b"original-fixture");
    let vault = fs::read(temp.vault()).unwrap();
    for args in [
        vec!["put", "svc/new", "--stdin"],
        vec!["put", "svc/existing", "--stdin", "--update"],
    ] {
        let mut producer = command(&temp)
            .args(["exec", "--", "molso-test-nonexistent-producer-73d8"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = producer.stdout.take().unwrap();
        let output = command(&temp).args(&args).stdin(input).output().unwrap();
        assert_eq!(producer.wait().unwrap().code(), Some(127));
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let hint = String::from_utf8_lossy(&output.stderr);
        assert!(hint.contains("stdin is empty") && hint.contains("--allow-empty"));
        assert_eq!(fs::read(temp.vault()).unwrap(), vault);
    }
    assert_eq!(run(&temp, &["list"]).stdout, b"svc/existing\n");
    assert_eq!(
        run(&temp, &["get", "svc/existing"]).stdout,
        b"original-fixture"
    );
}

#[test]
fn allow_empty_requires_stdin_before_reading_any_input() {
    let temp = Temp::new("allow-empty-option");
    let output = run(&temp, &["put", "svc/account", "--allow-empty"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--stdin"));
    assert!(!temp.key().exists() && !temp.vault().exists());
}

#[test]
fn help_supports_first_run_topics_and_unknown_topic_errors() {
    let temp = Temp::new("discoverability");
    let output = run(&temp, &[]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("--vault") && text.contains("--key-file"));
    assert!(text.contains("molso init") && text.contains("molso put"));
    let output = run(&temp, &["help", "exec"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--env"));
    let output = run(&temp, &["not-a-command", "--help"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!temp.key().exists() && !temp.vault().exists());
}

#[test]
fn global_paths_work_after_subcommands_and_equal_options_work() {
    let temp = Temp::new("argument-placement");
    let chosen_vault = temp.file("chosen-vault");
    let chosen_key = temp.file("chosen-key");
    let output = run(
        &temp,
        &[
            "init",
            "--vault",
            chosen_vault.to_str().unwrap(),
            "--key-file",
            chosen_key.to_str().unwrap(),
        ],
    );
    assert!(output.status.success());
    assert!(chosen_vault.exists() && chosen_key.exists());
    assert!(!temp.vault().exists() && !temp.key().exists());

    let vault_option = format!("--vault={}", chosen_vault.display());
    let key_option = format!("--key-file={}", chosen_key.display());
    let output = run_stdin(
        &temp,
        &["put", "a/b", "--stdin", &vault_option, &key_option],
        b"fixture",
    );
    assert!(output.status.success());
    let output = run(&temp, &["get", "a/b", &vault_option, &key_option]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"fixture");

    let out = temp.file("env-result");
    let output = helper_run(
        &temp,
        &["exec", "--env=TOK=a/b", &vault_option, &key_option, "--"],
        "env",
        &[("MOLSO_TEST_VAR", "TOK")],
        Some(b"fixture"),
        &out,
    );
    assert!(output.status.success());
    assert_eq!(result(&out), "MATCH");
}

#[test]
fn exec_without_credential_mappings_does_not_require_a_store() {
    let temp = Temp::new("exec-without-store");
    let out = temp.file("unused-result");
    let output = helper_run(
        &temp,
        &["exec", "--"],
        "exit",
        &[("MOLSO_TEST_EXIT", "0")],
        None,
        &out,
    );
    assert!(output.status.success());
    let missing = run(&temp, &["exec", "--", "definitely-not-a-real-command-xyz"]);
    assert_eq!(missing.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("PATH"));
    assert!(!temp.key().exists() && !temp.vault().exists());
    assert!(!temp.file("vault.lock").exists());
}

#[test]
fn exec_forwards_child_help_after_separator() {
    let temp = Temp::new("child-help");
    init(&temp);
    let helper = support::helper_program();
    let output = run(&temp, &["exec", "--", &helper, "--help"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--test-threads"));
}

#[test]
fn exec_missing_separator_explains_how_to_delimit_the_child() {
    let temp = Temp::new("exec-separator");
    let helper = support::helper_program();
    let marker = temp.file("started");
    let output = command(&temp)
        .args(["exec", "--stdin", "svc/account", &helper])
        .args(support::helper_args())
        .env("MOLSO_TEST_HELPER", "marker")
        .env("MOLSO_TEST_MARKER", &marker)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("place -- before the command"));
    assert!(!marker.exists());
    assert!(!temp.key().exists() && !temp.vault().exists());
}

#[test]
fn put_reports_setup_and_update_errors_before_reading_input() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp = Temp::new("put-preflight");
    for (args, hint) in [
        (vec!["put", "a/b", "--stdin"], "molso init"),
        (vec!["put", "a/b", "--stdin"], "--update"),
        (
            vec!["put", "missing/account", "--stdin", "--update"],
            "molso list",
        ),
    ] {
        let mut child = command(&temp)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            sender.send(child.wait_with_output().unwrap()).unwrap();
        });
        let early = receiver.recv_timeout(Duration::from_secs(5));
        // Keep stdin open until after the deadline; EOF must not be needed for an error.
        drop(stdin);
        let output = match early {
            Ok(output) => output,
            Err(_) => {
                waiter.join().unwrap();
                panic!("put waited for credential input before reporting a known error");
            }
        };
        waiter.join().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains(hint));
        assert!(output.stdout.is_empty());
        if !temp.key().exists() {
            init(&temp);
            put(&temp, "a/b", b"original-fixture");
        }
    }
    assert_eq!(run(&temp, &["get", "a/b"]).stdout, b"original-fixture");
}

#[test]
fn concurrent_puts_do_not_lose_updates() {
    let temp = Temp::new("concurrent");
    init(&temp);
    put(&temp, "seed/account", b"seed");

    let count = 8;
    let handles: Vec<_> = (0..count)
        .map(|index| {
            let vault = temp.vault();
            let key = temp.key();
            std::thread::spawn(move || {
                let mut child = Command::new(bin())
                    .env("MOLSO_VAULT", &vault)
                    .env("MOLSO_KEY_FILE", &key)
                    .args(["put", &format!("worker{index}/account"), "--stdin"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child.stdin.take().unwrap().write_all(b"value").unwrap();
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "worker {index} failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }

    let list = run(&temp, &["list"]);
    let names: Vec<&str> = std::str::from_utf8(&list.stdout).unwrap().lines().collect();
    assert_eq!(
        names.len(),
        count + 1,
        "all updates must survive: {names:?}"
    );
    assert!(names.contains(&"seed/account"));
    for index in 0..count {
        assert!(names.contains(&format!("worker{index}/account").as_str()));
    }
}

#[test]
fn exec_env_and_stdin_bytes() {
    let temp = Temp::new("exec");
    init(&temp);
    let secret = b"secret-value";
    put(&temp, "svc/account", secret);

    let env_out = temp.file("env.out");
    let env_output = helper_run(
        &temp,
        &["exec", "--env", "TOK=svc/account", "--"],
        "env",
        &[("MOLSO_TEST_VAR", "TOK")],
        Some(secret),
        &env_out,
    );
    assert!(env_output.status.success());
    assert_eq!(result(&env_out), "MATCH");
    assert!(!String::from_utf8_lossy(&env_output.stdout).contains("secret-value"));
    assert!(!String::from_utf8_lossy(&env_output.stderr).contains("secret-value"));

    let stdin_out = temp.file("stdin.out");
    let stdin_output = helper_run(
        &temp,
        &["exec", "--stdin", "svc/account", "--"],
        "stdin",
        &[],
        None,
        &stdin_out,
    );
    assert!(stdin_output.status.success());
    let expected = format!("LEN={} HASH={:016x}", secret.len(), support::fnv1a(secret));
    assert_eq!(result(&stdin_out), expected);
}

#[test]
fn exec_env_is_not_inherited_by_later_processes() {
    let temp = Temp::new("exec-inherit");
    init(&temp);
    put(&temp, "svc/account", b"value");

    let first_out = temp.file("first.out");
    let first = helper_run(
        &temp,
        &["exec", "--env", "MOLSO_TEST_TOK=svc/account", "--"],
        "presence",
        &[("MOLSO_TEST_VAR", "MOLSO_TEST_TOK")],
        Some(b"value"),
        &first_out,
    );
    assert!(first.status.success());
    assert_eq!(result(&first_out), "PRESENT");

    // A later, independent process must not see the injected variable.
    let second_out = temp.file("second.out");
    let status = Command::new(support::helper_program())
        .args(support::helper_args())
        .env("MOLSO_TEST_HELPER", "presence")
        .env("MOLSO_TEST_OUT", &second_out)
        .env("MOLSO_TEST_VAR", "MOLSO_TEST_TOK")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(result(&second_out), "ABSENT");
}

#[test]
fn exec_rejects_bad_env_before_spawn() {
    let temp = Temp::new("exec-bad");
    init(&temp);
    put(&temp, "svc/nonutf8", b"\xff\xfe");
    put(&temp, "svc/nul", b"nul\0value");
    put(&temp, "a/b", b"one");
    put(&temp, "c/d", b"two");

    // Control: a valid mapping must actually start the consumer and create the
    // marker, proving the marker mechanism can detect a spawned child.
    let control_marker = temp.file("control.marker");
    let control_out = temp.file("control.out");
    let control = helper_run(
        &temp,
        &["exec", "--env", "TOK=a/b", "--"],
        "marker",
        &[("MOLSO_TEST_MARKER", control_marker.to_str().unwrap())],
        None,
        &control_out,
    );
    assert!(control.status.success());
    assert!(control_marker.exists(), "control child must have run");

    for (label, prefix) in [
        ("nonutf8", vec!["exec", "--env", "TOK=svc/nonutf8", "--"]),
        ("nul", vec!["exec", "--env", "TOK=svc/nul", "--"]),
        (
            "duplicate",
            vec!["exec", "--env", "TOK=a/b", "--env", "TOK=c/d", "--"],
        ),
    ] {
        let marker = temp.file(&format!("{label}.marker"));
        let out = temp.file(&format!("{label}.out"));
        let output = helper_run(
            &temp,
            &prefix,
            "marker",
            &[("MOLSO_TEST_MARKER", marker.to_str().unwrap())],
            None,
            &out,
        );
        assert_eq!(
            output.status.code(),
            Some(125),
            "{label} must be a wrapper failure"
        );
        assert!(!marker.exists(), "{label} must not have started the child");
    }
}

#[test]
fn exec_exit_codes() {
    let temp = Temp::new("exec-codes");
    init(&temp);
    let out = temp.file("exit.out");

    let ok = helper_run(
        &temp,
        &["exec", "--"],
        "exit",
        &[("MOLSO_TEST_EXIT", "0")],
        None,
        &out,
    );
    assert_eq!(ok.status.code(), Some(0));
    let seven = helper_run(
        &temp,
        &["exec", "--"],
        "exit",
        &[("MOLSO_TEST_EXIT", "7")],
        None,
        &out,
    );
    assert_eq!(seven.status.code(), Some(7));
    // Reserved codes from the child are passed through unchanged.
    let reserved = helper_run(
        &temp,
        &["exec", "--"],
        "exit",
        &[("MOLSO_TEST_EXIT", "125")],
        None,
        &out,
    );
    assert_eq!(reserved.status.code(), Some(125));
    let missing = run(&temp, &["exec", "--", "definitely-not-a-real-command-xyz"]);
    assert_eq!(missing.status.code(), Some(127));
}

#[cfg(windows)]
mod windows {
    use super::*;

    #[test]
    fn exec_env_unicode_case_collision_is_rejected() {
        let temp = Temp::new("win-unicode-dup");
        init(&temp);
        put(&temp, "a/b", b"one");
        put(&temp, "c/d", b"two");
        let marker = temp.file("unicode.marker");
        let out = temp.file("unicode.out");
        // U+00E4 (ä) and U+00C4 (Ä) are the same environment name on Windows.
        let output = helper_run(
            &temp,
            &["exec", "--env", "\u{e4}=a/b", "--env", "\u{c4}=c/d", "--"],
            "marker",
            &[("MOLSO_TEST_MARKER", marker.to_str().unwrap())],
            None,
            &out,
        );
        assert_eq!(
            output.status.code(),
            Some(125),
            "Unicode case-insensitive duplicate must be a wrapper failure"
        );
        assert!(
            !marker.exists(),
            "child must not start on a duplicate mapping"
        );
    }

    #[test]
    fn exec_preserves_32bit_child_exit_values() {
        let temp = Temp::new("win-exit32");
        init(&temp);
        let out = temp.file("exit32.out");
        for code in ["300", "-1"] {
            let output = helper_run(
                &temp,
                &["exec", "--"],
                "exit",
                &[("MOLSO_TEST_EXIT", code)],
                None,
                &out,
            );
            let expected: i32 = code.parse().unwrap();
            assert_eq!(
                output.status.code(),
                Some(expected),
                "32-bit exit value {code} must be preserved"
            );
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn have(program: &str) -> bool {
        Command::new(program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn run_in_terminal(temp: &Temp, args: &[&str], input: Option<&str>) -> Output {
        let script = r#"
import errno, os, pty, select, signal, sys, tempfile, termios, time
pending = os.environ.pop("MOLSO_TEST_TERMINAL_INPUT", None)
with tempfile.TemporaryFile() as errors:
    pid, fd = pty.fork()
    if pid == 0:
        os.dup2(errors.fileno(), 2)
        os.execv(sys.argv[1], sys.argv[1:])
    data = b""
    deadline = time.monotonic() + 10
    try:
        while True:
            assert time.monotonic() < deadline, "terminal command timed out"
            if pending is not None and b"(input hidden): " in data:
                # The prompt can appear before echo is disabled.
                if not termios.tcgetattr(fd)[3] & termios.ECHO:
                    os.write(fd, pending.encode())
                    pending = None
            if not select.select([fd], [], [], 0.05)[0]:
                continue
            try:
                chunk = os.read(fd, 4096)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not chunk:
                break
            data += chunk
    except BaseException:
        os.kill(pid, signal.SIGKILL)
        raise
    finally:
        os.close(fd)
        _, status = os.waitpid(pid, 0)
    sys.stdout.buffer.write(data)
    errors.seek(0)
    sys.stderr.buffer.write(errors.read())
sys.exit(os.waitstatus_to_exitcode(status))
"#;
        let mut command = Command::new("python3");
        command
            .args(["-c", script, bin()])
            .args(args)
            .env("MOLSO_VAULT", temp.vault())
            .env("MOLSO_KEY_FILE", temp.key())
            .env_remove("MOLSO_TEST_TERMINAL_INPUT");
        if let Some(input) = input {
            command.env("MOLSO_TEST_TERMINAL_INPUT", input);
        }
        command.output().unwrap()
    }

    #[test]
    fn exec_signal_exit_code() {
        let temp = Temp::new("exec-signal");
        init(&temp);
        let signal = run(&temp, &["exec", "--", "sh", "-c", "kill -TERM $$"]);
        assert_eq!(signal.status.code(), Some(128 + 15));
    }

    #[test]
    fn exec_permission_denied_is_126() {
        let temp = Temp::new("exec-perm");
        init(&temp);
        let script = temp.file("not-executable.sh");
        fs::write(&script, b"#!/bin/sh\necho nope\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o600)).unwrap();
        let output = run(&temp, &["exec", "--", script.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(126));
    }

    #[test]
    fn exec_stdin_broken_pipe_is_reported() {
        if !have("python3") {
            eprintln!("skipping: python3 not available");
            return;
        }
        let temp = Temp::new("exec-pipe");
        init(&temp);
        let big = vec![b'x'; 512 * 1024];
        put(&temp, "svc/big", &big);

        let output = run(
            &temp,
            &[
                "exec",
                "--stdin",
                "svc/big",
                "--",
                "python3",
                "-c",
                "import os; os.close(0)",
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(125),
            "delivery failure must not report success"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("delivery"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("xxxx"));
    }

    #[test]
    fn put_hidden_terminal_input_names_entry_and_does_not_echo() {
        if !have("python3") {
            eprintln!("skipping: python3 not available");
            return;
        }
        let temp = Temp::new("hidden-input");
        init(&temp);
        let output = run_in_terminal(
            &temp,
            &["put", "svc/account"],
            Some("fictional-pty-value\n"),
        );
        assert!(
            output.status.success(),
            "hidden-input pty check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("Credential for svc/account"));
        for stream in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(stream).contains("fictional-pty-value"));
        }
        assert_eq!(
            run(&temp, &["get", "svc/account"]).stdout,
            b"fictional-pty-value"
        );
    }

    #[test]
    fn empty_terminal_input_neither_creates_nor_overwrites_credentials() {
        if !have("python3") {
            eprintln!("skipping: python3 not available");
            return;
        }
        let temp = Temp::new("empty-terminal-input");
        init(&temp);
        put(&temp, "svc/existing", b"original-fixture");
        let vault = fs::read(temp.vault()).unwrap();
        for args in [
            vec!["put", "svc/new"],
            vec!["put", "svc/existing", "--update"],
        ] {
            let output = run_in_terminal(&temp, &args, Some("\n"));
            assert_eq!(output.status.code(), Some(2));
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("empty credential not saved"), "{error}");
            assert!(error.contains("rerun put"));
            assert_eq!(fs::read(temp.vault()).unwrap(), vault);
        }
        assert_eq!(run(&temp, &["list"]).stdout, b"svc/existing\n");
        assert_eq!(
            run(&temp, &["get", "svc/existing"]).stdout,
            b"original-fixture"
        );
    }

    #[test]
    fn empty_terminal_list_offers_next_step_only_on_stderr() {
        if !have("python3") {
            eprintln!("skipping: python3 not available");
            return;
        }
        let temp = Temp::new("empty-terminal-list");
        init(&temp);
        let empty = run_in_terminal(&temp, &["list"], None);
        assert!(empty.status.success());
        assert!(empty.stdout.is_empty());
        let hint = String::from_utf8_lossy(&empty.stderr);
        assert!(hint.contains("no credentials stored"));
        assert!(hint.contains("molso put SERVICE/ACCOUNT"));

        put(&temp, "svc/account", b"fictional-value");
        let nonempty = run_in_terminal(&temp, &["list"], None);
        assert!(nonempty.status.success());
        assert_eq!(nonempty.stdout, b"svc/account\r\n");
        assert!(nonempty.stderr.is_empty());
    }

    #[test]
    fn get_refuses_terminal() {
        if !have("python3") {
            eprintln!("skipping: python3 not available");
            return;
        }
        let temp = Temp::new("tty");
        init(&temp);
        put(&temp, "svc/account", b"terminal-secret");

        let output = run_in_terminal(&temp, &["get", "svc/account"], None);
        assert_eq!(output.status.code(), Some(2), "get on a terminal must fail");
        assert!(output.stdout.is_empty());
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("terminal-secret"),
            "refusal must not leak the credential"
        );
    }
}
