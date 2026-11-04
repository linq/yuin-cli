use std::process::{Command, Stdio};

fn yuin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_yuin"));
    command.stdin(Stdio::null());
    command
}

#[test]
fn redirected_default_is_help_instead_of_a_tui() {
    let output = yuin().output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Usage: yuin"));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn json_commands_produce_one_machine_readable_document() {
    for command in ["doctor", "paths"] {
        let output = yuin().args([command, "--json"]).output().unwrap();
        assert!(output.status.success());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(value.is_object());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn explicit_tui_rejects_non_terminal_input_without_control_sequences() {
    let output = yuin().arg("tui").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("interactive terminal"));
    assert!(!error.contains('\u{1b}'));
}

#[test]
fn unknown_commands_return_a_usage_error() {
    let output = yuin().arg("not-a-command").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}
