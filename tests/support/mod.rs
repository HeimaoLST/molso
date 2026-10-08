//! Shared integration-test support. This module is compiled only into the
//! integration test binary, never into the shipped `molso` binary.
//!
//! The integration test binary doubles as a cross-platform consumer process
//! for `molso exec` tests. Cargo/libtest has no plain "program" mode, so the
//! consumer logic lives in an ignored test (`helper`) that is invoked as:
//!
//! `molso exec --env TOK=a/b -- <test-binary> --ignored --exact --nocapture helper`
//!
//! The helper reads its role from `MOLSO_TEST_HELPER` and writes its result to
//! the file named by `MOLSO_TEST_OUT`, so libtest's own stdout banner does not
//! interfere with the assertion.

use std::io::Read;

pub const HELPER_TEST: &str = "helper";

pub fn helper_program() -> String {
    std::env::current_exe()
        .expect("test binary path")
        .to_str()
        .expect("test binary path is UTF-8")
        .to_string()
}

pub fn helper_args() -> [&'static str; 4] {
    ["--ignored", "--exact", "--nocapture", HELPER_TEST]
}

pub fn fnv1a(data: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn read_stdin() -> Vec<u8> {
    let mut data = Vec::new();
    std::io::stdin().read_to_end(&mut data).unwrap();
    data
}

pub fn helper_main() {
    let Ok(role) = std::env::var("MOLSO_TEST_HELPER") else {
        return;
    };
    let variable = std::env::var("MOLSO_TEST_VAR").unwrap_or_default();
    let output = match role.as_str() {
        "env" => {
            let expected = read_stdin();
            let matched = std::env::var_os(&variable)
                .is_some_and(|value| value.as_encoded_bytes() == expected.as_slice());
            if matched { "MATCH" } else { "MISMATCH" }.to_string()
        }
        "stdin" => {
            let data = read_stdin();
            format!("LEN={} HASH={:016x}", data.len(), fnv1a(&data))
        }
        "presence" => if std::env::var_os(&variable).is_some() {
            "PRESENT"
        } else {
            "ABSENT"
        }
        .to_string(),
        "marker" => {
            let path = std::env::var("MOLSO_TEST_MARKER").expect("MOLSO_TEST_MARKER");
            std::fs::write(path, b"started").unwrap();
            "OK".to_string()
        }
        "exit" => {
            let code = std::env::var("MOLSO_TEST_EXIT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            std::process::exit(code);
        }
        other => format!("UNKNOWN:{other}"),
    };
    if let Ok(path) = std::env::var("MOLSO_TEST_OUT") {
        std::fs::write(path, output).unwrap();
    }
}
