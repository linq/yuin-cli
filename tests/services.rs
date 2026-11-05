#![cfg(unix)]
use std::process::Command;
#[test]
fn complete_services_flow_uses_only_local_fixtures() {
    let result = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/services.py"
        ))
        .env("YUIN_TEST_BINARY", env!("CARGO_BIN_EXE_yuin"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
