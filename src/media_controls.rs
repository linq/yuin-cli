use crate::{
    app::Envelope,
    config::Config,
    domain::{PlaybackState, Snapshot},
    protocol::AppCommand,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{RwLock, mpsc, oneshot, watch};

pub struct MediaSession {
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
impl MediaSession {
    pub async fn shutdown(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

pub async fn start(
    commands: mpsc::Sender<Envelope>,
    snapshots: watch::Receiver<Snapshot>,
    config: Arc<RwLock<Config>>,
) -> Option<MediaSession> {
    if !config.read().await.media_keys {
        return None;
    }
    let (sender, mut receiver) = mpsc::channel(32);
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        use souvlaki::{
            MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
        };
        let result = (|| {
            let mut controls = MediaControls::new(PlatformConfig {
                dbus_name: "yuin",
                display_name: "Yuin CLI",
                hwnd: None,
            })?;
            controls.attach(move |event| {
                let _ = sender.try_send(event);
            })?;
            let mut last_track = None;
            let mut last_duration = None;
            loop {
                if !matches!(
                    stop_rx.recv_timeout(Duration::from_millis(250)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ) {
                    break;
                }
                let snapshot = snapshots.borrow().clone();
                if snapshot.current != last_track || snapshot.duration != last_duration {
                    let track = snapshot.current_entry().map(|e| &e.track);
                    controls.set_metadata(MediaMetadata {
                        title: track.map(|t| t.title.as_str()),
                        artist: track.map(|t| t.artist.as_str()),
                        album: track.map(|t| t.album.as_str()),
                        cover_url: None,
                        duration: snapshot.duration.map(Duration::from_secs_f64),
                    })?;
                    last_track = snapshot.current;
                    last_duration = snapshot.duration;
                }
                let progress = Some(MediaPosition(Duration::from_secs_f64(
                    snapshot.position.max(0.0),
                )));
                let state = match snapshot.state {
                    PlaybackState::Playing => MediaPlayback::Playing { progress },
                    PlaybackState::Paused | PlaybackState::Buffering | PlaybackState::Resolving => {
                        MediaPlayback::Paused { progress }
                    }
                    _ => MediaPlayback::Stopped,
                };
                controls.set_playback(state)?;
            }
            Ok::<_, souvlaki::Error>(())
        })();
        if let Err(error) = result {
            eprintln!("System media controls unavailable: {error}");
        }
    });
    let (session_stop, mut stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            let event = tokio::select! {_=&mut stopped=>break,event=receiver.recv()=>event};
            let Some(event) = event else {
                break;
            };
            if !config.read().await.media_keys {
                continue;
            }
            let snapshot = match crate::daemon::call(AppCommand::Snapshot, false).await {
                Ok(snapshot) => snapshot,
                Err(_) => break,
            };
            let Some(command) = command(event, &snapshot) else {
                continue;
            };
            let (reply, result) = oneshot::channel();
            if commands
                .send(Envelope {
                    command,
                    reply,
                    prepared: None,
                    detached: false,
                })
                .await
                .is_err()
            {
                break;
            }
            let _ = result.await;
        }
        let _ = stop_tx.send(());
        let _ = tokio::task::spawn_blocking(move || thread.join()).await;
    });
    Some(MediaSession {
        stop: session_stop,
        task,
    })
}

fn command(event: souvlaki::MediaControlEvent, snapshot: &Snapshot) -> Option<AppCommand> {
    use souvlaki::{MediaControlEvent as E, SeekDirection};
    Some(match event {
        E::Play => AppCommand::Resume,
        E::Pause => AppCommand::Pause,
        E::Toggle => {
            if snapshot.state == PlaybackState::Playing {
                AppCommand::Pause
            } else {
                AppCommand::Resume
            }
        }
        E::Next => AppCommand::Next,
        E::Previous => AppCommand::Previous,
        E::Stop => AppCommand::Stop,
        E::SetPosition(position) => AppCommand::Seek {
            seconds: position.0.as_secs_f64(),
        },
        E::Seek(direction) => AppCommand::Seek {
            seconds: (snapshot.position
                + if direction == SeekDirection::Forward {
                    5.0
                } else {
                    -5.0
                })
            .max(0.0),
        },
        E::SeekBy(direction, amount) => AppCommand::Seek {
            seconds: (snapshot.position
                + if direction == SeekDirection::Forward {
                    amount.as_secs_f64()
                } else {
                    -amount.as_secs_f64()
                })
            .max(0.0),
        },
        E::SetVolume(volume) if volume.is_finite() => AppCommand::Volume {
            percent: (volume.clamp(0.0, 1.0) * 100.0).round() as u8,
        },
        // OS requests never supply arbitrary paths, URLs, shell commands or daemon shutdown.
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
pub fn run_mac(
    runtime: &tokio::runtime::Runtime,
    future: impl std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
) -> anyhow::Result<()> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::{NSDate, NSRunLoop};
    let marker = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("Media controls require the main thread"))?;
    let app = NSApplication::sharedApplication(marker);
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    app.finishLaunching();
    let task = runtime.spawn(future);
    while !task.is_finished() {
        objc2::rc::autoreleasepool(|_| {
            NSRunLoop::mainRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.02));
        });
    }
    runtime.block_on(task)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_keys_map_to_allowed_app_commands() {
        let state = Snapshot::default();
        assert!(matches!(
            command(souvlaki::MediaControlEvent::Toggle, &state),
            Some(AppCommand::Resume)
        ));
        assert!(
            command(
                souvlaki::MediaControlEvent::OpenUri("file:///tmp/input".into()),
                &state
            )
            .is_none()
        );
        assert!(matches!(
            command(souvlaki::MediaControlEvent::SetVolume(10.0), &state),
            Some(AppCommand::Volume { percent: 100 })
        ));
    }
}
