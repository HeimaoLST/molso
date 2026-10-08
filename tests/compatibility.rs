use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct TestDir(PathBuf);

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_molso"));
    command
        .env("MOLSO_VAULT", directory.join("vault"))
        .env("MOLSO_KEY_FILE", directory.join("key"));
    command
}

#[test]
fn macos_format_v1_fixture_can_be_read_and_updated_on_each_platform() {
    let directory = TestDir(std::env::temp_dir().join(format!(
        "molso-compatibility-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    )));
    fs::create_dir(&directory.0).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/format-v1");
    for file in ["key", "vault"] {
        fs::copy(fixtures.join(file), directory.0.join(file)).unwrap();
    }
    let expected = b"molso-cross-platform-fixture\x00\xff\r\n";
    let output = command(&directory.0)
        .args(["get", "fixture/account"])
        .output()
        .unwrap();
    assert!(output.status.success(), "the reference vault must decrypt");
    assert!(
        output.stdout == expected,
        "the reference value must preserve all bytes"
    );

    let mut child = command(&directory.0)
        .args(["put", "fixture/new-account", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"new-fictional-value")
        .unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    let output = command(&directory.0)
        .args(["get", "fixture/account"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        output.stdout == expected,
        "a native update must keep the reference entry readable"
    );
}
