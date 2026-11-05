#![cfg(unix)]
use std::{
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn an_infinite_script_is_interrupted_when_the_supervisor_disconnects() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("loop.js");
    std::fs::write(&script, "while(true){}").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_yuin"))
        .arg("source-worker")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(
        input,
        "{{\"id\":\"bounded\",\"op\":\"init\",\"meta\":{{}}}}"
    )
    .unwrap();
    input.flush().unwrap();
    thread::sleep(Duration::from_millis(100));
    assert!(child.try_wait().unwrap().is_none());
    drop(input);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                matches!(status.code(), Some(0 | 1)),
                "Worker ended by signal instead of cooperative cancellation: {status}"
            );
            break;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Worker ignored supervisor disconnect while evaluating an infinite script");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn oversized_source_is_rejected_before_execution() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("large.js");
    let file = std::fs::File::create(&script).unwrap();
    file.set_len(9_000_001).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_yuin"))
        .arg("source-worker")
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("script size"));
}
