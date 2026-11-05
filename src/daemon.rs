use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Semaphore, broadcast, mpsc, oneshot, watch},
    task::JoinSet,
    time::{sleep, timeout},
};

use crate::{
    app::{Core, Envelope},
    domain::Snapshot,
    paths::AppPaths,
    protocol::{
        AppCommand, AppEvent, MAX_FRAME, MAX_RESPONSE_FRAME, Request, Response, RpcError, VERSION,
        read_frame, read_frame_buffered_with_limit, read_frame_with_limit,
    },
    secure,
    storage::Storage,
};

const RPC_TIMEOUT: Duration = Duration::from_secs(120);

pub async fn call(command: AppCommand, autostart: bool) -> Result<Snapshot> {
    serde_json::from_value(call_value(command, autostart).await?).map_err(Into::into)
}

pub async fn call_value(command: AppCommand, autostart: bool) -> Result<serde_json::Value> {
    let paths = AppPaths::discover()?;
    let mut stream = connect(&paths, autostart).await?;
    let request = Request::new(command);
    send(&mut stream, &request).await?;
    receive(&mut BufReader::new(stream), request.request_id).await
}

async fn receive(
    reader: &mut BufReader<UnixStream>,
    request_id: uuid::Uuid,
) -> Result<serde_json::Value> {
    let frame = timeout(
        RPC_TIMEOUT,
        read_frame_with_limit(reader, MAX_RESPONSE_FRAME),
    )
    .await
    .context("Daemon response timed out")??
    .context("Daemon disconnected")?;
    let response: Response = serde_json::from_slice(&frame).context("Invalid daemon response")?;
    // A daemon predating a command rejects it during deserialization; older
    // builds also lose the request ID. Explain how to upgrade, while rejecting
    // unrelated responses with an incorrect non-nil ID.
    if response.version == VERSION
        && (response.request_id.is_nil() || response.request_id == request_id)
        && response.error.as_ref().is_some_and(|error| {
            error.code == 2 && error.message == "Invalid request JSON or method"
        })
    {
        return Err(RpcError::new(
            6,
            "Background service cannot read this command. If Yuin was updated, run `yuin daemon stop`, then retry with the updated Yuin executable.",
        ).into());
    }
    ensure!(
        response.version == VERSION && response.request_id == request_id,
        "Daemon protocol mismatch"
    );
    if let Some(error) = response.error {
        return Err(error.into());
    }
    response.result.context("Daemon returned no result")
}

pub struct Subscription {
    reader: BufReader<UnixStream>,
    frame: Vec<u8>,
}

impl Subscription {
    pub async fn connect(autostart: bool) -> Result<(Self, Snapshot)> {
        let mut stream = connect(&AppPaths::discover()?, autostart).await?;
        let request = Request::new(AppCommand::Subscribe);
        send(&mut stream, &request).await?;
        let mut reader = BufReader::new(stream);
        let snapshot = serde_json::from_value(receive(&mut reader, request.request_id).await?)?;
        Ok((
            Self {
                reader,
                frame: Vec::new(),
            },
            snapshot,
        ))
    }

    pub async fn next(&mut self) -> Result<Snapshot> {
        let frame =
            read_frame_buffered_with_limit(&mut self.reader, &mut self.frame, MAX_RESPONSE_FRAME)
                .await?
                .context("Daemon disconnected; reconnect to restore the session")?;
        let event: AppEvent = serde_json::from_slice(&frame)?;
        ensure!(
            event.version == VERSION && event.sequence == event.snapshot.sequence,
            "Invalid daemon event"
        );
        Ok(event.snapshot)
    }
}

async fn connect(paths: &AppPaths, autostart: bool) -> Result<UnixStream> {
    if paths.runtime.exists() {
        secure::check_directory(&paths.runtime)?;
    }
    match connect_once(paths).await {
        Ok(stream) => return Ok(stream),
        Err(error) if unavailable(&error) => {}
        Err(error) => return Err(error),
    }
    if !autostart {
        return Err(RpcError::new(6, "Daemon is not running").into());
    }
    secure::create_directory(&paths.runtime)?;
    // Each concurrent starter may spawn a contender; only one can acquire the file lock.
    let log_path = paths.runtime.join("daemon.log");
    let log = secure::private_file(&log_path)?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    for _ in 0..150 {
        match connect_once(paths).await {
            Ok(stream) => return Ok(stream),
            Err(error) if unavailable(&error) => sleep(Duration::from_millis(30)).await,
            Err(error) => return Err(error),
        }
    }
    Err(RpcError::new(
        6,
        format!("Daemon did not start; inspect {}", log_path.display()),
    )
    .into())
}

fn unavailable(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
    })
}

async fn connect_once(paths: &AppPaths) -> Result<UnixStream> {
    secure::check_socket(&paths.socket())?;
    let stream = timeout(Duration::from_secs(2), UnixStream::connect(paths.socket())).await??;
    ensure!(
        stream.peer_cred()?.uid() == secure::uid(),
        "Daemon belongs to another user"
    );
    Ok(stream)
}

async fn send(
    writer: &mut (impl AsyncWriteExt + Unpin),
    value: &impl serde::Serialize,
) -> Result<()> {
    send_with_limit(writer, value, MAX_FRAME).await
}

async fn send_response(
    writer: &mut (impl AsyncWriteExt + Unpin),
    value: &impl serde::Serialize,
) -> Result<()> {
    send_with_limit(writer, value, MAX_RESPONSE_FRAME).await
}

async fn send_with_limit(
    writer: &mut (impl AsyncWriteExt + Unpin),
    value: &impl serde::Serialize,
    limit: usize,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() <= limit,
        "IPC frame exceeds {} MiB",
        limit / (1024 * 1024)
    );
    timeout(Duration::from_secs(2), writer.write_all(&bytes)).await??;
    Ok(())
}

struct SocketGuard(std::path::PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub async fn run() -> Result<()> {
    // All daemon-created files, including SQLite journals, are private at creation.
    unsafe {
        libc::umask(0o077);
    }
    let paths = AppPaths::discover()?;
    secure::create_directory(&paths.runtime)?;
    let lock = secure::private_file(&paths.runtime.join("daemon.lock"))?;
    // Keep the descriptor alive until the core and its children have been stopped.
    lock.try_lock()
        .context("Another Yuin daemon already owns this runtime")?;
    if paths.socket().symlink_metadata().is_ok() {
        secure::check_socket(&paths.socket())?;
        match UnixStream::connect(paths.socket()).await {
            Ok(_) => anyhow::bail!("A live process owns the IPC endpoint; refusing to remove it"),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                fs::remove_file(paths.socket())?
            }
            Err(error) => return Err(error.into()),
        }
    }
    secure::create_directory(&paths.data)?;
    let database = paths.data.join("library.sqlite3");
    secure::private_file(&database)?;
    let storage = Storage::open(database).await?;
    let (event_tx, _) = broadcast::channel(32);
    let (snapshot_tx, snapshot_rx) = watch::channel(Snapshot::default());
    let (command_tx, command_rx) = mpsc::channel(64);
    let services = Arc::new(crate::services::Services::new(paths.clone(), storage.clone()).await?);
    let core = Core::new(storage, event_tx.clone(), snapshot_tx, services.clone()).await?;
    let listener = UnixListener::bind(paths.socket())?;
    let _socket = SocketGuard(paths.socket());
    fs::set_permissions(paths.socket(), fs::Permissions::from_mode(0o600))?;
    let (stop_tx, stop_rx) = oneshot::channel();
    let signals = tokio::spawn(async {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { result = tokio::signal::ctrl_c() => result?, _ = terminate.recv() => {} }
        let _ = stop_tx.send(());
        Ok::<_, std::io::Error>(())
    });
    let media_controls = crate::media_controls::start(
        command_tx.clone(),
        snapshot_rx.clone(),
        services.config.clone(),
    )
    .await;
    let mut core = tokio::spawn(core.run(command_rx, stop_rx));
    let permits = Arc::new(Semaphore::new(32));
    let mut connections = JoinSet::new();
    let result = loop {
        tokio::select! {
            result = &mut core => break result?,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if stream.peer_cred()?.uid() != secure::uid() { continue; }
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                let commands = command_tx.clone();
                let events = event_tx.subscribe();
                let snapshot = snapshot_rx.clone();
                let services=services.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve(stream, commands, events, snapshot,services).await {
                        // Do not log request bodies or playback URLs.
                        eprintln!("IPC client disconnected: {error}");
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
        }
    };
    if let Some(controls) = media_controls {
        controls.shutdown().await;
    }
    services.downloads.shutdown().await;
    services.sources.stop().await;
    signals.abort();
    // Let the shutdown request's response flush before closing clients.
    sleep(Duration::from_millis(100)).await;
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    result
}

async fn serve(
    stream: UnixStream,
    commands: mpsc::Sender<Envelope>,
    mut events: broadcast::Receiver<Snapshot>,
    latest: watch::Receiver<Snapshot>,
    services: Arc<crate::services::Services>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let Some(frame) = timeout(Duration::from_secs(5), read_frame(&mut reader)).await?? else {
        return Ok(());
    };
    let request: Request = match serde_json::from_slice(&frame) {
        Ok(request) => request,
        Err(_) => {
            // Even an unknown method/service can have a valid envelope. Keep
            // its identity so clients can surface the actual rejection.
            let request_id = serde_json::from_slice::<serde_json::Value>(&frame)
                .ok()
                .and_then(|value| value["request_id"].as_str().and_then(|id| id.parse().ok()))
                .unwrap_or_else(uuid::Uuid::nil);
            send_response(
                &mut write,
                &Response {
                    version: VERSION,
                    request_id,
                    result: None,
                    error: Some(RpcError::new(2, "Invalid request JSON or method")),
                },
            )
            .await?;
            return Ok(());
        }
    };
    if request.version != VERSION {
        send_response(
            &mut write,
            &Response {
                version: VERSION,
                request_id: request.request_id,
                result: None,
                error: Some(RpcError::new(2, "Unsupported protocol version")),
            },
        )
        .await?;
        return Ok(());
    }
    if let AppCommand::Service { command } = request.command {
        let result = tokio::select! {
            result=timeout(RPC_TIMEOUT,services.execute(command))=>result.map_err(|_|anyhow::anyhow!("Service request timed out")).and_then(|r|r),
            _=reader.fill_buf()=>return Ok(()),
        };
        let (result, error) = match result {
            Ok(value) => (Some(value), None),
            Err(error) => {
                let code = error.downcast_ref::<RpcError>().map_or(1, |e| e.code);
                (None, Some(RpcError::new(code, error.to_string())))
            }
        };
        send_response(
            &mut write,
            &Response {
                version: VERSION,
                request_id: request.request_id,
                result,
                error,
            },
        )
        .await?;
        return Ok(());
    }
    let (command, detached) = if let AppCommand::Detached { command } = request.command {
        if !matches!(
            *command,
            AppCommand::Play { .. }
                | AppCommand::PlayCollection { .. }
                | AppCommand::PlayLibrary { .. }
                | AppCommand::PlayEntry { .. }
                | AppCommand::Resume
                | AppCommand::Next
                | AppCommand::Previous
        ) {
            send_response(
                &mut write,
                &Response {
                    version: VERSION,
                    request_id: request.request_id,
                    result: None,
                    error: Some(RpcError::new(2, "Only playback loads may be detached")),
                },
            )
            .await?;
            return Ok(());
        }
        (*command, true)
    } else {
        (request.command, false)
    };
    let subscribe = matches!(command, AppCommand::Subscribe);
    let readonly = subscribe || matches!(command, AppCommand::Snapshot);
    let (reply, mut result) = oneshot::channel();
    let dispatch = async {
        let prepared_result = match &command {
            AppCommand::Play { path } | AppCommand::QueueAdd { path } => {
                services.track(path).await.map(|track| Some(vec![track]))
            }
            AppCommand::PlayCollection { id, chart } => {
                services.collection_tracks(id, *chart).await.map(Some)
            }
            AppCommand::PlayLibrary { id } => services.library_tracks(*id).await.map(Some),
            _ => Ok(None),
        };
        let prepared = match prepared_result {
            Ok(value) => value,
            Err(error) => {
                return Ok(Err(error
                    .downcast_ref::<RpcError>()
                    .cloned()
                    .unwrap_or_else(|| RpcError::new(6, error.to_string()))));
            }
        };
        commands
            .send(Envelope {
                command,
                prepared,
                detached,
                reply,
            })
            .await?;
        let response = (&mut result).await?;
        Ok::<_, anyhow::Error>(response)
    };
    // Dropping result cancels requests which have not entered the core yet.
    // Read-only requests remain available while the core waits for backend I/O.
    let response = if readonly {
        Ok(latest.borrow().clone())
    } else {
        tokio::select! {
            response = timeout(RPC_TIMEOUT, dispatch) => response??,
            _ = reader.fill_buf() => return Ok(()),
        }
    };
    let (snapshot, error) = match response {
        Ok(snapshot) => (Some(snapshot), None),
        Err(error) => (None, Some(error)),
    };
    let mut sequence = snapshot.as_ref().map_or(0, |s| s.sequence);
    let success = error.is_none();
    send_response(
        &mut write,
        &Response {
            version: VERSION,
            request_id: request.request_id,
            result: snapshot.map(serde_json::to_value).transpose()?,
            error,
        },
    )
    .await?;
    if !subscribe || !success {
        return Ok(());
    }
    loop {
        let snapshot = tokio::select! {
            _ = reader.fill_buf() => return Ok(()),
            event = events.recv() => match event {
                Ok(snapshot) => snapshot,
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Every event is a complete snapshot; reconnect obtains a fresh sequence.
                    return Ok(());
                }
            }
        };
        if snapshot.sequence <= sequence {
            continue;
        }
        sequence = snapshot.sequence;
        send_response(
            &mut write,
            &AppEvent {
                version: VERSION,
                sequence,
                snapshot,
            },
        )
        .await?;
    }
    Ok(())
}

use tokio::io::AsyncBufReadExt;
