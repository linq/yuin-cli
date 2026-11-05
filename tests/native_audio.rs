#![cfg(unix)]
use std::process::Command;
#[test]
fn native_audio_streaming_failures_and_cancellation() {
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/native-audio.py"
        ))
        .env("YUIN_TEST_BINARY", env!("CARGO_BIN_EXE_yuin"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
