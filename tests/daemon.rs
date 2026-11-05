#![cfg(unix)]

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Session {
    root: TempDir,
    data: PathBuf,
    runtime: PathBuf,
    mpv: PathBuf,
}

impl Session {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let output = Command::new(env!("CARGO_BIN_EXE_yuin"))
            .env("YUIN_DATA_DIR", &data)
            .args(["paths", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        Self {
            root,
            data,
            runtime: PathBuf::from(value["runtime"].as_str().unwrap()),
            mpv: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mpv.py"),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_yuin"));
        command
            .env("YUIN_DATA_DIR", &self.data)
            .env("YUIN_MPV", &self.mpv)
            .env("YUIN_FIXTURE_MARKER", self.root.path().join("starts"))
            .stdin(Stdio::null());
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).arg("--json").output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn audio(&self, name: &str) -> PathBuf {
        let path = self.root.path().join(name);
        // A generated 60-second PCM WAV; also usable for the opt-in real mpv test.
        let size = 8000_u32 * 2 * 60;
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(size + 36).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&8000_u32.to_le_bytes()).unwrap();
        file.write_all(&16000_u32.to_le_bytes()).unwrap();
        file.write_all(&2_u16.to_le_bytes()).unwrap();
        file.write_all(&16_u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&size.to_le_bytes()).unwrap();
        file.write_all(&vec![0_u8; size as usize]).unwrap();
        path
    }

    fn stop(&self) {
        let _ = self.output(&["daemon", "stop"]);
        let start = Instant::now();
        while self.runtime.join("daemon.sock").exists() && start.elapsed() < Duration::from_secs(3)
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!self.runtime.join("daemon.sock").exists());
    }

    fn wait_state(&self, state: &str) -> Value {
        let start = Instant::now();
        loop {
            let snapshot = self.json(&["status"]);
            if snapshot["state"] == state {
                return snapshot;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "Wanted {state}, got {snapshot}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn request(&self, value: Value) -> Value {
        let mut socket = UnixStream::connect(self.runtime.join("daemon.sock")).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(socket, "{value}").unwrap();
        let mut line = String::new();
        BufReader::new(socket).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.runtime);
    }
}

#[test]
fn read_only_commands_and_invalid_arguments_have_no_side_effects() {
    let session = Session::new();
    for args in [
        vec!["paths"],
        vec!["doctor"],
        vec!["--help"],
        vec!["--version"],
        vec!["tui"],
        vec!["volume", "101"],
        vec!["seek", "NaN"],
        vec!["seek", "inf"],
    ] {
        let output = session.command().args(&args).output().unwrap();
        if ["volume", "seek"].contains(&args[0]) {
            assert_eq!(output.status.code(), Some(2));
        }
        assert!(!session.data.exists());
        assert!(!session.runtime.exists());
    }
    let stopped = session.output(&["daemon", "stop"]);
    assert_eq!(stopped.status.code(), Some(6));
    assert!(!session.runtime.exists());
}

#[test]
fn concurrent_clients_share_one_queue_and_transactional_recovery() {
    let session = Session::new();
    let audio = session.audio("重复 中文.wav");
    let mut children = Vec::new();
    for _ in 0..8 {
        children.push(
            session
                .command()
                .args(["queue", "add", audio.to_str().unwrap(), "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let queue = session.json(&["queue", "list"]);
    let entries = queue["queue"].as_array().unwrap();
    assert_eq!(entries.len(), 8);
    let mut ids: Vec<_> = entries
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 8);
    assert!(
        entries
            .windows(2)
            .all(|pair| pair[0]["track"]["id"] == pair[1]["track"]["id"])
    );
    assert_eq!(
        fs::metadata(&session.runtime).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(session.runtime.join("daemon.sock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    session.stop();
    assert_eq!(session.json(&["status"])["queue"], queue["queue"]);
    let duplicate = session.command().args(["daemon", "run"]).output().unwrap();
    assert!(!duplicate.status.success());
    assert_eq!(session.json(&["status"])["queue"], queue["queue"]);
}

#[test]
fn playback_controls_persist_and_restore_without_starting_mpv() {
    let session = Session::new();
    let audio = session.audio("local audio.wav");
    let playing = session.json(&["play", audio.to_str().unwrap()]);
    assert_eq!(playing["state"], "playing");
    session.wait_state("playing");
    assert_eq!(session.json(&["pause"])["state"], "paused");
    assert_eq!(session.json(&["seek", "17"])["position"], 17.0);
    assert_eq!(session.json(&["volume", "42"])["volume"], 42);
    session.stop();
    let starts = fs::read_to_string(session.root.path().join("starts")).unwrap();
    let restored = session.json(&["status"]);
    assert_eq!(restored["state"], "paused");
    assert_eq!(restored["position"], 17.0);
    assert_eq!(restored["volume"], 42);
    assert_eq!(restored["current"], playing["current"]);
    assert_eq!(
        fs::read_to_string(session.root.path().join("starts")).unwrap(),
        starts
    );
    assert_eq!(session.json(&["resume"])["state"], "playing");
    session.json(&["queue", "add", audio.to_str().unwrap()]);
    let next = session.json(&["next"]);
    assert_ne!(next["current"], playing["current"]);
    assert_eq!(session.json(&["previous"])["current"], playing["current"]);
    let first = playing["current"].as_str().unwrap();
    assert_eq!(session.json(&["queue", "remove", first])["state"], "idle");
    session.json(&["resume"]);
    assert_eq!(session.json(&["stop"])["state"], "idle");
    assert_eq!(
        session.json(&["queue", "clear"])["queue"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn missing_backend_and_decode_failure_return_audio_errors() {
    for missing in [true, false] {
        let mut session = Session::new();
        if missing {
            session.mpv = session.root.path().join("missing-mpv");
        }
        let audio = session.audio("invalid-audio.wav");
        let output = session.output(&["play", audio.to_str().unwrap()]);
        assert_eq!(
            output.status.code(),
            Some(5),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["schema_version"], 1);
        assert_eq!(error["error"]["code"], 5);
        let state = session.wait_state("error");
        assert!(state["error"].is_string());
        assert_eq!(state["queue"].as_array().unwrap().len(), 1);
        assert_eq!(session.json(&["queue", "clear"])["state"], "idle");
    }
}

#[test]
fn legacy_daemon_rejection_explains_how_to_restart() {
    use std::os::unix::net::UnixListener;
    let session = Session::new();
    fs::create_dir(&session.runtime).unwrap();
    fs::set_permissions(&session.runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = session.runtime.join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["params"]["command"]["op"], "search_collections");
        writeln!(stream, "{}", json!({"version":1,"request_id":"00000000-0000-0000-0000-000000000000","error":{"code":2,"message":"Invalid request JSON or method"}})).unwrap();
    });
    let output = session.output(&["playlists", "search", "fixture"]);
    server.join().unwrap();
    fs::remove_file(socket).unwrap();
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], 6);
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("daemon stop"),
        "{error}"
    );
}

#[test]
fn unknown_service_preserves_request_identity() {
    let session = Session::new();
    session.json(&["status"]);
    let id = "7000d328-94fb-4b96-9861-75424d9297b0";
    let response = session.request(json!({"version":1,"request_id":id,"method":"service","params":{"command":{"op":"future_search"}}}));
    assert_eq!(response["request_id"], id);
    assert_eq!(response["error"]["code"], 2);
}

#[test]
fn protocol_rejects_bad_requests_and_bounds_input() {
    let session = Session::new();
    session.json(&["status"]);
    let id = "7000d328-94fb-4b96-9861-75424d9297b0";
    for request in [
        json!({"version":99,"request_id":id,"method":"snapshot"}),
        json!({"version":1,"request_id":id,"method":"volume","params":{"percent":101}}),
        json!({"version":1,"request_id":id,"method":"run","params":{"command":"touch /tmp/no"}}),
        json!({"version":1,"request_id":id,"method":"seek","params":{"seconds":-1}}),
    ] {
        assert_eq!(session.request(request)["error"]["code"], 2);
    }
    let mut socket = UnixStream::connect(session.runtime.join("daemon.sock")).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let _ = socket.write_all(&vec![b'x'; 1024 * 1024 + 1]);
    use std::io::Read;
    let mut byte = [0];
    assert!(matches!(socket.read(&mut byte), Ok(0) | Err(_)));
    assert_eq!(session.json(&["status"])["state"], "idle");
}

#[test]
fn subscriptions_send_ordered_snapshots_and_backend_exit_is_recoverable() {
    let session = Session::new();
    session.json(&["status"]);
    let mut stream = UnixStream::connect(session.runtime.join("daemon.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(stream, "{}", json!({"version":1,"request_id":"7000d328-94fb-4b96-9861-75424d9297b0","method":"subscribe"})).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let initial: Value = serde_json::from_str(&line).unwrap();
    session.json(&["volume", "25"]);
    line.clear();
    reader.read_line(&mut line).unwrap();
    let event: Value = serde_json::from_str(&line).unwrap();
    assert!(event["sequence"].as_u64().unwrap() > initial["result"]["sequence"].as_u64().unwrap());
    assert_eq!(event["snapshot"]["volume"], 25);
    let audio = session.audio("crash-on-seek.wav");
    session.json(&["play", audio.to_str().unwrap()]);
    let output = session.output(&["seek", "4"]);
    assert_eq!(output.status.code(), Some(5));
    session.wait_state("error");
    assert_eq!(session.json(&["resume"])["state"], "playing");
}

#[test]
fn eof_advances_the_core_queue() {
    let session = Session::new();
    let audio = session.audio("first.wav");
    let second = session.audio("second.wav");
    let first = session.json(&["play", audio.to_str().unwrap()]);
    session.json(&["queue", "add", second.to_str().unwrap()]);
    session.json(&["seek", "60"]);
    let start = Instant::now();
    loop {
        let snapshot = session.json(&["status"]);
        if snapshot["current"] != first["current"] && snapshot["state"] == "playing" {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{snapshot}");
    }
    session.json(&["seek", "60"]);
    session.wait_state("ended");
}

#[test]
fn unsafe_runtime_directory_is_rejected() {
    let session = Session::new();
    fs::create_dir(&session.runtime).unwrap();
    fs::set_permissions(&session.runtime, fs::Permissions::from_mode(0o777)).unwrap();
    let output = session.output(&["status"]);
    assert!(!output.status.success());
    assert!(!session.data.exists());
    fs::set_permissions(&session.runtime, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
#[ignore = "requires installed mpv and explicit real-backend opt-in"]
fn real_mpv_controls_and_restart() {
    let mut session = Session::new();
    session.mpv = std::env::var_os("YUIN_REAL_MPV")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("mpv").into());
    let audio = session.audio("real silence.wav");
    assert_eq!(
        session.json(&["play", audio.to_str().unwrap()])["state"],
        "playing"
    );
    session.wait_state("playing");
    thread::sleep(Duration::from_millis(500));
    let playing = session.json(&["status"]);
    assert!(playing["position"].as_f64().unwrap() > 0.1);
    assert_eq!(session.json(&["pause"])["state"], "paused");
    session.json(&["seek", "12"]);
    thread::sleep(Duration::from_millis(100));
    let paused = session.json(&["status"]);
    assert!(
        (paused["position"].as_f64().unwrap() - 12.0).abs() < 0.2,
        "{paused}"
    );
    session.stop();
    let restored = session.json(&["status"]);
    assert_eq!(restored["state"], "paused");
    assert!((restored["position"].as_f64().unwrap() - 12.0).abs() < 0.2);
    session.json(&["resume"]);
    assert_eq!(session.json(&["stop"])["state"], "idle");
}

#[test]
fn a_disconnected_loading_request_is_cancelled_and_status_remains_responsive() {
    let session = Session::new();
    session.json(&["status"]);
    let audio = session.audio("slow-start.wav");
    let mut stream = UnixStream::connect(session.runtime.join("daemon.sock")).unwrap();
    writeln!(stream, "{}", json!({"version":1,"request_id":"7000d328-94fb-4b96-9861-75424d9297b0","method":"play","params":{"path":audio}})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !session.root.path().join("starts").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let start = Instant::now();
    assert_eq!(session.json(&["status"])["state"], "buffering");
    assert!(start.elapsed() < Duration::from_millis(500));
    drop(stream);
    let state = session.wait_state("idle");
    assert!(state["queue"].as_array().unwrap().is_empty());
    session.stop();
    assert!(
        session.json(&["status"])["queue"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn killing_the_daemon_closes_mpv_and_stale_socket_is_recovered() {
    let session = Session::new();
    let mut daemon = session
        .command()
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !session.runtime.join("daemon.sock").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let audio = session.audio("owned.wav");
    session.json(&["play", audio.to_str().unwrap()]);
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !session.root.path().join("starts.exits").exists() {
        assert!(
            Instant::now() < deadline,
            "Player did not exit when daemon connection closed"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let recovered = session.json(&["status"]);
    assert_eq!(recovered["state"], "paused");
    assert_eq!(recovered["queue"].as_array().unwrap().len(), 1);
    assert_eq!(
        fs::read_to_string(session.root.path().join("starts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn data_directory_aliases_use_the_same_daemon() {
    let session = Session::new();
    session.json(&["status"]);
    let alias = session.root.path().join("alias");
    std::os::unix::fs::symlink(&session.data, &alias).unwrap();
    let output = session
        .command()
        .env("YUIN_DATA_DIR", &alias)
        .args(["paths", "--json"])
        .output()
        .unwrap();
    let paths: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        paths["runtime"].as_str().unwrap(),
        session.runtime.to_str().unwrap()
    );
    let output = session
        .command()
        .env("YUIN_DATA_DIR", &alias)
        .args(["volume", "33", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(session.json(&["status"])["volume"], 33);
}

#[test]
fn pause_interrupts_loading_without_blocking_and_mode_survives_restart() {
    let session = Session::new();
    let audio = session.audio("slow-start.wav");
    let child = session
        .command()
        .args(["play", audio.to_str().unwrap(), "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while !session.root.path().join("starts").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let start = Instant::now();
    assert_eq!(session.json(&["pause"])["state"], "paused");
    assert!(start.elapsed() < Duration::from_millis(500));
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(session.json(&["status"])["state"], "paused");
    session.json(&["mode", "shuffle"]);
    session.stop();
    assert_eq!(session.json(&["status"])["mode"], "shuffle");
}

#[test]
fn fade_can_be_interrupted_by_stop_without_leaving_old_audio() {
    let session = Session::new();
    let first = session.audio("first-fade.wav");
    let second = session.audio("second-fade.wav");
    session.json(&["config", "set", "fade_seconds", "0.3"]);
    session.json(&["play", first.to_str().unwrap()]);
    session.json(&["play", second.to_str().unwrap()]);
    assert_eq!(session.json(&["stop"])["state"], "idle");
    let pids = fs::read_to_string(session.root.path().join("starts")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let living = pids
            .lines()
            .any(|pid| unsafe { libc::kill(pid.parse().unwrap(), 0) } == 0);
        if !living {
            break;
        }
        assert!(Instant::now() < deadline, "Old fade player survived stop");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn an_accepted_detached_load_survives_its_ui_connection_closing() {
    let session = Session::new();
    session.json(&["status"]);
    let audio = session.audio("slow-start.wav");
    let request = json!({"version":1,"request_id":"7000d328-94fb-4b96-9861-75424d9297b0","method":"detached","params":{"command":{"method":"play","params":{"path":audio}}}});
    let start = Instant::now();
    let response = session.request(request);
    assert!(response["error"].is_null(), "{response}");
    assert!(start.elapsed() < Duration::from_millis(500));
    session.wait_state("playing");
}

#[test]
fn cancelled_playlist_load_restores_old_queue_and_keeps_concurrent_additions() {
    let session = Session::new();
    let old = session.audio("old.wav");
    let slow = session.audio("slow-start.wav");
    let next = session.audio("other.wav");
    let added = session.audio("concurrent.wav");
    let before = session.json(&["play", old.to_str().unwrap()]);
    let id = session.json(&["library", "create", "batch"])["playlist"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    session.json(&["library", "add", &id, slow.to_str().unwrap()]);
    session.json(&["library", "add", &id, next.to_str().unwrap()]);
    let mut socket = UnixStream::connect(session.runtime.join("daemon.sock")).unwrap();
    writeln!(socket,"{}",json!({"version":1,"request_id":"7000d328-94fb-4b96-9861-75424d9297b0","method":"play_library","params":{"id":id}})).unwrap();
    session.wait_state("buffering");
    let during = session.json(&["queue", "add", added.to_str().unwrap()]);
    assert_eq!(during["queue"].as_array().unwrap().len(), 3);
    drop(socket);
    let after = session.wait_state("paused");
    let queue = after["queue"].as_array().unwrap();
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0], before["queue"][0]);
    assert_eq!(
        queue[1]["track"]["path"],
        added.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(after["current"], before["current"]);
}
