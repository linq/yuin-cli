//! In-process decoding and output; the core owns queue identity and generations.
mod decoder;
mod input;

use anyhow::{Context, Result, bail};
use rodio::Source;
use std::{
    collections::BTreeMap,
    num::{NonZeroU16, NonZeroU32},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc as sync,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum BackendEvent {
    Loaded,
    Position { seconds: f64, observed_at: Instant },
    Duration(f64),
    Buffering(bool),
    Ended,
    Failed(String),
}
pub struct Event {
    pub generation: u64,
    pub event: BackendEvent,
}

#[derive(Default)]
struct Controls {
    paused: AtomicBool,
    volume: AtomicU8,
    epoch: AtomicU64,
    position: AtomicU64,
    buffering: AtomicBool,
    eof: AtomicBool,
    ended: AtomicBool,
}
struct Chunk {
    epoch: u64,
    position: u64,
    samples: Vec<f32>,
}
struct Pcm {
    chunks: sync::Receiver<Chunk>,
    current: Option<Chunk>,
    offset: usize,
    channels: NonZeroU16,
    rate: NonZeroU32,
    controls: Arc<Controls>,
    cancel: CancellationToken,
}
impl Iterator for Pcm {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.cancel.is_cancelled() {
            return None;
        }
        if self.controls.paused.load(Ordering::Acquire) {
            return Some(0.0);
        }
        let epoch = self.controls.epoch.load(Ordering::Acquire);
        loop {
            if let Some(chunk) = &self.current
                && chunk.epoch == epoch
                && self.offset < chunk.samples.len()
            {
                let sample = chunk.samples[self.offset];
                self.offset += 1;
                self.controls.position.store(
                    chunk.position + self.offset as u64 / self.channels.get() as u64,
                    Ordering::Release,
                );
                self.controls.buffering.store(false, Ordering::Release);
                return Some(sample * self.controls.volume.load(Ordering::Relaxed) as f32 / 100.0);
            }
            match self.chunks.try_recv() {
                Ok(chunk) => {
                    self.current = Some(chunk);
                    self.offset = 0;
                }
                Err(_) => {
                    self.current = None;
                    if self.controls.eof.load(Ordering::Acquire) {
                        self.controls.ended.store(true, Ordering::Release);
                        return None;
                    }
                    // No HTTP, decoding or command waits on the audio callback.
                    self.controls.buffering.store(true, Ordering::Release);
                    return Some(0.0);
                }
            }
        }
    }
}
impl Source for Pcm {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> NonZeroU16 {
        self.channels
    }
    fn sample_rate(&self) -> NonZeroU32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
struct Seek {
    seconds: f64,
    reply: oneshot::Sender<Result<()>>,
}
pub struct Player {
    controls: Arc<Controls>,
    seeks: sync::Sender<Seek>,
    cancel: CancellationToken,
    worker: Option<std::thread::JoinHandle<()>>,
    monitor: tokio::task::JoinHandle<()>,
}
impl Player {
    pub async fn load(
        path: &str,
        headers: &BTreeMap<String, String>,
        position: f64,
        volume: u8,
        generation: u64,
        events: mpsc::Sender<Event>,
    ) -> Result<Self> {
        let cancel = CancellationToken::new();
        let guard = cancel.clone().drop_guard();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let input = tokio::time::timeout_at(deadline, input::open(path, headers, cancel.clone()))
            .await
            .context("Audio loading timed out")??;
        let controls = Arc::new(Controls::default());
        controls.volume.store(volume, Ordering::Release);
        let (seeks, commands) = sync::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (faults, mut failures) = mpsc::unbounded_channel();
        let thread_controls = controls.clone();
        let thread_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("yuin-audio".into())
            .spawn(move || {
                let mut ready = Some(ready_tx);
                let result = run_audio(
                    input,
                    position,
                    thread_controls,
                    &thread_cancel,
                    commands,
                    &mut ready,
                    faults.clone(),
                );
                if let Err(error) = result {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Err(error));
                    } else if !thread_cancel.is_cancelled() {
                        let _ = faults.send(format!("Audio playback failed: {error:#}"));
                    }
                }
            })?;
        let ready = tokio::time::timeout_at(deadline, ready_rx)
            .await
            .context("Audio loading timed out")?
            .context("Audio worker exited during loading")??;
        let rate = ready.0;
        if let Some(duration) = ready.1 {
            let _ = events
                .send(Event {
                    generation,
                    event: BackendEvent::Duration(duration),
                })
                .await;
        }
        let _ = events
            .send(Event {
                generation,
                event: BackendEvent::Loaded,
            })
            .await;
        let observed = controls.clone();
        let monitor_cancel = cancel.clone();
        let monitor = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(Duration::from_millis(100));
            let mut buffering = false;
            loop {
                let event = tokio::select! {
                    _ = monitor_cancel.cancelled() => break,
                    Some(error) = failures.recv() => BackendEvent::Failed(error),
                    _ = ticks.tick() => {
                        if observed.ended.load(Ordering::Acquire) { BackendEvent::Ended }
                        else {
                            let now = observed.buffering.load(Ordering::Acquire);
                            if now != buffering { buffering = now; BackendEvent::Buffering(now) }
                            else {
                                // Timestamp before sampling so a concurrent seek can reject
                                // observations made before it completed.
                                let observed_at = Instant::now();
                                BackendEvent::Position {
                                    seconds: observed.position.load(Ordering::Acquire) as f64 / rate as f64,
                                    observed_at,
                                }
                            }
                        }
                    }
                };
                let terminal = matches!(event, BackendEvent::Ended | BackendEvent::Failed(_));
                if events.send(Event { generation, event }).await.is_err() || terminal {
                    break;
                }
            }
        });
        guard.disarm();
        Ok(Self {
            controls,
            seeks,
            cancel,
            worker: Some(worker),
            monitor,
        })
    }
    pub async fn pause(&mut self, paused: bool) -> Result<()> {
        self.controls.paused.store(paused, Ordering::Release);
        Ok(())
    }
    pub async fn volume(&mut self, percent: u8) -> Result<()> {
        self.controls.volume.store(percent, Ordering::Release);
        Ok(())
    }
    pub async fn seek(&mut self, seconds: f64) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.seeks
            .send(Seek { seconds, reply })
            .context("Audio worker disconnected")?;
        tokio::time::timeout(Duration::from_secs(8), result)
            .await
            .context("Audio seek timed out")?
            .context("Audio worker disconnected during seek")?
    }
    pub async fn fade_out(mut self, seconds: f64, volume: u8) {
        let steps = (seconds * 20.0).ceil().max(1.0) as u32;
        for step in 1..=steps {
            tokio::time::sleep(Duration::from_secs_f64(seconds / steps as f64)).await;
            let _ = self
                .volume((volume as f64 * (1.0 - step as f64 / steps as f64)).round() as u8)
                .await;
        }
        self.shutdown().await;
    }
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        self.monitor.abort();
        if let Some(worker) = self.worker.take() {
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        }
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.monitor.abort();
    }
}
type Ready = oneshot::Sender<Result<(u32, Option<f64>)>>;

fn run_audio(
    input: input::Input,
    position: f64,
    controls: Arc<Controls>,
    cancel: &CancellationToken,
    commands: sync::Receiver<Seek>,
    ready: &mut Option<Ready>,
    faults: mpsc::UnboundedSender<String>,
) -> Result<()> {
    let mut decoder = decoder::Decoder::new(input.source, input.progressive)?;
    if position > 0.0 {
        decoder.seek(position)?;
    }
    let first = decoder
        .next()?
        .context("File contains no decodable audio")?;
    let channels = NonZeroU16::new(decoder.channels).context("Invalid channel count")?;
    let rate = NonZeroU32::new(decoder.rate).context("Invalid sample rate")?;
    let (samples, chunks) = sync::sync_channel(8);
    let output_cancel = cancel.child_token();
    let _output_guard = output_cancel.clone().drop_guard();
    let pcm = Pcm {
        chunks,
        current: None,
        offset: 0,
        channels,
        rate,
        controls: controls.clone(),
        cancel: output_cancel,
    };
    let mut headless = None;
    // Diagnostic mode uses the real decoder and PCM clock, never an automatic fallback.
    let _device = match std::env::var("YUIN_AUDIO_OUTPUT").as_deref() {
        Ok("null") => {
            headless = Some(std::thread::spawn(move || {
                let mut pcm = pcm;
                let step = rate.get() as usize * channels.get() as usize / 100;
                let mut deadline = Instant::now();
                loop {
                    if (0..step).any(|_| pcm.next().is_none()) {
                        break;
                    }
                    deadline += Duration::from_millis(10);
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                }
            }));
            None
        }
        Ok(value) if value != "default" => {
            bail!("Unknown audio output: {value}; use default or null")
        }
        _ => {
            let mut device = rodio::DeviceSinkBuilder::from_default_device()
                .context("No system audio output device is available")?
                .with_error_callback(move |error| {
                    let _ = faults.send(format!("Audio output device failed: {error}"));
                })
                .open_stream()
                .context("Cannot open the system audio output device")?;
            device.log_on_drop(false);
            device.mixer().add(pcm);
            Some(device)
        }
    };
    let mut pending = Some(first);
    if let Some(ready) = ready.take() {
        let _ = ready.send(Ok((rate.get(), decoder.duration)));
    }
    let mut epoch = 0;
    while !cancel.is_cancelled() {
        if let Ok(command) = commands.try_recv() {
            let result = decoder.seek(command.seconds);
            if result.is_ok() {
                epoch += 1;
                controls.eof.store(false, Ordering::Release);
                controls.ended.store(false, Ordering::Release);
                controls.epoch.store(epoch, Ordering::Release);
                controls.position.store(
                    (command.seconds * rate.get() as f64) as u64,
                    Ordering::Release,
                );
                pending = None;
            }
            let failed = result.is_err();
            let _ = command.reply.send(result);
            if failed {
                bail!("Cannot seek this audio stream");
            }
        }
        if pending.is_none() && !controls.eof.load(Ordering::Acquire) {
            pending = decoder.next()?;
            if pending.is_none() {
                controls.eof.store(true, Ordering::Release);
            }
        }
        if let Some((position, data)) = pending.take() {
            match samples.try_send(Chunk {
                epoch,
                position,
                samples: data,
            }) {
                Ok(()) => continue,
                Err(sync::TrySendError::Full(chunk)) => {
                    pending = Some((chunk.position, chunk.samples))
                }
                Err(sync::TrySendError::Disconnected(_)) => break,
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if let Some(headless) = headless {
        let _ = headless.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pcm_pause_volume_seek_generation_starvation_and_cancellation() {
        let controls = Arc::new(Controls::default());
        controls.volume.store(50, Ordering::Release);
        let cancel = CancellationToken::new();
        let (tx, chunks) = sync::sync_channel(4);
        let mut pcm = Pcm {
            chunks,
            current: None,
            offset: 0,
            channels: NonZeroU16::new(2).unwrap(),
            rate: NonZeroU32::new(48000).unwrap(),
            controls: controls.clone(),
            cancel: cancel.clone(),
        };
        assert_eq!(pcm.next(), Some(0.0));
        assert!(controls.buffering.load(Ordering::Acquire));
        tx.send(Chunk {
            epoch: 0,
            position: 0,
            samples: vec![0.8, -0.8, 0.4, -0.4],
        })
        .unwrap();
        assert_eq!(pcm.next(), Some(0.4));
        controls.paused.store(true, Ordering::Release);
        assert_eq!(pcm.next(), Some(0.0));
        assert_eq!(controls.position.load(Ordering::Acquire), 0);
        controls.paused.store(false, Ordering::Release);
        assert_eq!(pcm.next(), Some(-0.4));
        assert_eq!(controls.position.load(Ordering::Acquire), 1);
        controls.epoch.store(1, Ordering::Release);
        tx.send(Chunk {
            epoch: 0,
            position: 99,
            samples: vec![1.0, 1.0],
        })
        .unwrap();
        tx.send(Chunk {
            epoch: 1,
            position: 48000,
            samples: vec![0.2, -0.2],
        })
        .unwrap();
        assert_eq!(pcm.next(), Some(0.1));
        assert_eq!(controls.position.load(Ordering::Acquire), 48000);
        cancel.cancel();
        assert_eq!(pcm.next(), None);
    }
}
