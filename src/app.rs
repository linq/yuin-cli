use std::{collections::HashSet, time::Duration};

use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    time::{Instant, interval},
};
use uuid::Uuid;

use crate::{
    domain::{PlaybackMode, PlaybackState, QueueEntry, Snapshot},
    playback::{BackendEvent, Event, Mpv},
    protocol::{AppCommand, MAX_QUEUE, MAX_RESPONSE_FRAME, RpcError},
    storage::Storage,
};

pub struct Envelope {
    pub command: AppCommand,
    pub prepared: Option<Vec<crate::domain::Track>>,
    pub detached: bool,
    pub reply: oneshot::Sender<Result<Snapshot, RpcError>>,
}

struct Loaded {
    generation: u64,
    result: Result<(Mpv, Option<Mpv>, f64, u8), RpcError>,
    track: crate::domain::Track,
}
struct Pending {
    reply: oneshot::Sender<Result<Snapshot, RpcError>>,
    before: Snapshot,
    added: HashSet<Uuid>,
    replaced: bool,
}

pub struct Core {
    services: std::sync::Arc<crate::services::Services>,
    state: Snapshot,
    storage: Storage,
    backend: Option<Mpv>,
    loading: Option<tokio::task::JoinHandle<()>>,
    loaded: mpsc::Receiver<Loaded>,
    load_sender: mpsc::Sender<Loaded>,
    pending: Option<Pending>,
    outgoing: Option<tokio::task::JoinHandle<()>>,
    backend_events: mpsc::Receiver<Event>,
    backend_sender: mpsc::Sender<Event>,
    events: broadcast::Sender<Snapshot>,
    latest: watch::Sender<Snapshot>,
    generation: u64,
    desired_pause: bool,
    last_position: Instant,
    checkpoint_position: f64,
}

impl Core {
    pub async fn new(
        storage: Storage,
        events: broadcast::Sender<Snapshot>,
        latest: watch::Sender<Snapshot>,
        services: std::sync::Arc<crate::services::Services>,
    ) -> anyhow::Result<Self> {
        let state = storage.load().await?;
        latest.send_replace(state.clone());
        let (backend_sender, backend_events) = mpsc::channel(128);
        let checkpoint_position = state.position;
        let (load_sender, loaded) = mpsc::channel(4);
        Ok(Self {
            services,
            state,
            storage,
            backend: None,
            loading: None,
            loaded,
            load_sender,
            pending: None,
            outgoing: None,
            backend_events,
            backend_sender,
            events,
            latest,
            generation: 0,
            desired_pause: true,
            last_position: Instant::now(),
            checkpoint_position,
        })
    }

    pub async fn run(
        mut self,
        mut commands: mpsc::Receiver<Envelope>,
        mut shutdown: oneshot::Receiver<()>,
    ) -> anyhow::Result<()> {
        let mut tick = interval(Duration::from_millis(250));
        let mut checkpoint = interval(Duration::from_secs(5));
        let result = loop {
            tokio::select! {
                _ = &mut shutdown => break Ok(()),
                envelope = commands.recv() => {
                    let Some(envelope) = envelope else { break Ok(()); };
                    if envelope.reply.is_closed() { continue; }
                    let stop = matches!(envelope.command, AppCommand::Shutdown);
                    let replaced=matches!(envelope.command,AppCommand::PlayCollection{..}|AppCommand::PlayLibrary{..});
                    let waits=!envelope.detached && matches!(envelope.command,AppCommand::Play{..}|AppCommand::PlayEntry{..}|AppCommand::PlayCollection{..}|AppCommand::PlayLibrary{..}|AppCommand::Resume|AppCommand::Next|AppCommand::Previous);
                    let before = self.state.clone();
                    let mut reply = envelope.reply;
                    let result = tokio::select! {
                        result = self.handle(envelope.command,envelope.prepared) => Some(result),
                        _ = reply.closed() => None,
                    };
                    let Some(result) = result else {
                        self.stop_backend().await;
                        let sequence = self.state.sequence;
                        self.state = before;
                        self.state.sequence = sequence;
                        self.state.state = if self.state.current.is_some() { PlaybackState::Paused } else { PlaybackState::Idle };
                        self.storage.save(&self.state).await?;
                        self.publish();
                        continue;
                    };
                    let success = result.is_ok();
                    if waits && success && self.loading.is_some(){
                        if let Some(previous)=self.pending.take(){let _=previous.reply.send(Err(RpcError::new(6,"Playback request superseded")));}
                        let old_ids=before.queue.iter().map(|entry|entry.id).collect::<HashSet<_>>();
                        let added=self.state.queue.iter().filter(|entry|!old_ids.contains(&entry.id)).map(|entry|entry.id).collect();
                        self.pending=Some(Pending{reply,before,added,replaced});
                    }else{let _ = reply.send(result);}
                    if self.loading.is_none() && let Some(pending)=self.pending.take(){let _=pending.reply.send(Ok(self.state.clone()));}
                    if stop && success { break Ok(()); }
                }
                Some(loaded)=self.loaded.recv()=>{if let Err(error)=self.finish_load(loaded).await{break Err(error);}}
                Some(event) = self.backend_events.recv() => {
                    if event.generation == self.generation { self.backend_event(event.event).await; }
                }
                _ = tick.tick() => {
                    if self.pending.as_ref().is_some_and(|pending|pending.reply.is_closed()) {
                        let pending=self.pending.take().expect("pending checked");self.cancel_pending(pending).await?;
                    }
                    if self.state.state == PlaybackState::Playing {
                        self.advance_position();
                        self.publish();
                    }
                }
                _ = checkpoint.tick() => {
                    if (self.state.position - self.checkpoint_position).abs() >= 1.0 {
                        if let Err(error) = self.storage.checkpoint(&self.state).await { break Err(error); }
                        self.checkpoint_position = self.state.position;
                    }
                }
            }
        };
        self.advance_position();
        self.stop_backend().await;
        let saved = self.storage.checkpoint(&self.state).await;
        result.and(saved)
    }

    async fn finish_load(&mut self, loaded: Loaded) -> anyhow::Result<()> {
        if loaded.generation != self.generation {
            return Ok(());
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.reply.is_closed())
        {
            let pending = self.pending.take().expect("pending checked");
            return self.cancel_pending(pending).await;
        }
        self.loading.take();
        let mut result = match loaded.result {
            Ok((backend, outgoing, fade, volume)) => {
                self.backend = Some(backend);
                if let Some(old) = outgoing {
                    self.outgoing = Some(tokio::spawn(old.fade_out(fade, volume)));
                }
                self.state.state = if self.desired_pause {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                };
                self.last_position = Instant::now();
                self.storage
                    .history_add(loaded.track)
                    .await
                    .map_err(|e| RpcError::new(1, e.to_string()))
            }
            Err(error) => Err(error),
        };
        if let Some(backend) = &mut self.backend
            && let Err(error) = backend.volume(self.state.volume).await
        {
            result = Err(audio_error(error));
        }
        if let Err(error) = &result {
            self.stop_backend().await;
            self.state.state = PlaybackState::Error;
            self.state.error = Some(error.message.clone());
        }
        self.storage.checkpoint(&self.state).await?;
        self.publish();
        if let Some(pending) = self.pending.take() {
            let _ = pending.reply.send(result.map(|_| self.state.clone()));
        }
        Ok(())
    }

    async fn cancel_pending(&mut self, pending: Pending) -> anyhow::Result<()> {
        self.stop_backend().await;
        self.state
            .queue
            .retain(|entry| !pending.added.contains(&entry.id));
        if pending.replaced {
            let appended = std::mem::take(&mut self.state.queue);
            self.state.queue = pending.before.queue.clone();
            let mut seen = self
                .state
                .queue
                .iter()
                .map(|entry| entry.id)
                .collect::<HashSet<_>>();
            for entry in appended {
                if seen.insert(entry.id) {
                    self.state.queue.push(entry);
                }
            }
        }
        self.state.current = pending
            .before
            .current
            .filter(|id| self.state.queue.iter().any(|entry| entry.id == *id));
        self.state.position = if self.state.current.is_some() {
            pending.before.position
        } else {
            0.0
        };
        self.state.duration = None;
        self.state.state = if self.state.current.is_some() {
            PlaybackState::Paused
        } else {
            PlaybackState::Idle
        };
        self.state.error = None;
        self.storage.save(&self.state).await?;
        self.publish();
        Ok(())
    }

    fn advance_position(&mut self) {
        let now = Instant::now();
        if self.state.state == PlaybackState::Playing {
            self.state.position += now.duration_since(self.last_position).as_secs_f64();
            if let Some(duration) = self.state.duration {
                self.state.position = self.state.position.min(duration);
            }
        }
        self.last_position = now;
    }

    fn publish(&mut self) {
        self.state.sequence += 1;
        self.latest.send_replace(self.state.clone());
        let _ = self.events.send(self.state.clone());
    }

    async fn stop_outgoing(&mut self) {
        if let Some(task) = self.outgoing.take() {
            task.abort();
            let _ = task.await;
        }
    }

    async fn stop_backend(&mut self) {
        self.stop_outgoing().await;
        if let Some(task) = self.loading.take() {
            task.abort();
            let _ = task.await;
        }
        self.generation += 1;
        if let Some(backend) = self.backend.take() {
            backend.shutdown().await;
        }
    }

    async fn start_current(&mut self, position: f64) -> Result<(), RpcError> {
        self.stop_outgoing().await;
        let fade = self.services.config.read().await.fade_seconds;
        let outgoing = if fade > 0.0 && self.state.state == PlaybackState::Playing {
            self.generation += 1;
            self.backend.take()
        } else {
            self.stop_backend().await;
            None
        };
        let track = self
            .state
            .current_entry()
            .ok_or_else(|| RpcError::new(6, "No current queue entry"))?
            .track
            .clone();
        self.state.state = PlaybackState::Resolving;
        self.state.position = position;
        self.state.duration = None;
        self.state.error = None;
        self.desired_pause = false;
        self.publish();
        let services = self.services.clone();
        let generation = self.generation;
        let events = self.backend_sender.clone();
        let finished = self.load_sender.clone();
        let volume = self.state.volume;
        self.loading = Some(tokio::spawn(async move {
            let result = async {
                let media = services.resolve(&track, None).await.map_err(|error| {
                    error
                        .downcast_ref::<RpcError>()
                        .cloned()
                        .unwrap_or_else(|| RpcError::new(5, error.to_string()))
                })?;
                let _ = events
                    .send(Event {
                        generation,
                        event: BackendEvent::Buffering(true),
                    })
                    .await;
                let backend = Mpv::load(
                    &media.url,
                    &media.headers,
                    position,
                    volume,
                    generation,
                    events.clone(),
                )
                .await
                .map_err(audio_error)?;
                Ok((backend, outgoing, fade, volume))
            }
            .await;
            let _ = finished
                .send(Loaded {
                    generation,
                    result,
                    track,
                })
                .await;
        }));
        Ok(())
    }

    async fn handle(
        &mut self,
        command: AppCommand,
        prepared: Option<Vec<crate::domain::Track>>,
    ) -> Result<Snapshot, RpcError> {
        self.advance_position();
        if matches!(command, AppCommand::Snapshot | AppCommand::Subscribe) {
            return Ok(self.state.clone());
        }
        let before = self.state.clone();
        let mut queue_changed = false;
        let play = matches!(command, AppCommand::Play { .. });
        let previous = matches!(command, AppCommand::Previous);
        let clear = matches!(command, AppCommand::QueueClear);
        let action = async {
            match command {
                AppCommand::PlayCollection { .. } | AppCommand::PlayLibrary { .. } => {
                    let tracks=prepared.ok_or_else(||RpcError::new(2,"Missing prepared playlist"))?;
                    if tracks.is_empty() {return Err(RpcError::new(2,"Playlist is empty"));}
                    if tracks.len()>MAX_QUEUE {return Err(RpcError::new(2,format!("Playlist has {} tracks; playback queue supports at most {MAX_QUEUE}. Current queue retained.",tracks.len())));}
                    let mut candidate=self.state.clone();
                    candidate.queue=tracks.into_iter().map(|track|QueueEntry{id:Uuid::new_v4(),track}).collect();
                    let index=if candidate.mode==PlaybackMode::Shuffle {rand::random_range(0..candidate.queue.len())} else {0};
                    candidate.current=Some(candidate.queue[index].id);
                    candidate.position=0.0;
                    candidate.duration=None;
                    candidate.error=None;
                    if serde_json::to_vec(&candidate).map_err(|e|RpcError::new(1,e.to_string()))?.len()>MAX_RESPONSE_FRAME-4096 {return Err(RpcError::new(2,"Playlist exceeds IPC snapshot size limit. Current queue retained."));}
                    self.state=candidate;
                    queue_changed=true;
                    self.start_current(0.0).await?;
                }
                AppCommand::Play { .. } | AppCommand::QueueAdd { .. } => {
                    if self.state.queue.len() >= MAX_QUEUE {
                        return Err(RpcError::new(2, format!("Queue limit is {MAX_QUEUE} entries")));
                    }
                    let entry = QueueEntry {
                        id: Uuid::new_v4(),
                        track: prepared.and_then(|mut tracks| tracks.pop())
                            .ok_or_else(|| RpcError::new(2, "Missing prepared track metadata"))?,
                    };
                    let id = entry.id;
                    self.state.queue.push(entry);
                    if !play && let Some(pending)=&self.pending && pending.replaced {
                        let mut rollback=pending.before.clone();
                        rollback.queue.extend(self.state.queue.iter().filter(|entry|!pending.added.contains(&entry.id)).cloned());
                        if rollback.queue.len()>MAX_QUEUE || serde_json::to_vec(&rollback).map_err(|error|RpcError::new(1,error.to_string()))?.len()>MAX_RESPONSE_FRAME-4096 {
                            self.state=before.clone();
                            return Err(RpcError::new(2,"Queue addition exceeds capacity while playlist playback is loading"));
                        }
                    }
                    if serde_json::to_vec(&self.state)
                        .map_err(|e| RpcError::new(1, e.to_string()))?
                        .len()
                        > MAX_RESPONSE_FRAME - 4096
                    {
                        self.state = before.clone();
                        return Err(RpcError::new(2, "Queue exceeds IPC snapshot size limit"));
                    }
                    queue_changed = true;
                    if play {
                        self.state.current = Some(id);
                        self.start_current(0.0).await?;
                    }
                }
                AppCommand::PlayEntry { id } => {
                    if !self.state.queue.iter().any(|entry| entry.id == id) {
                        return Err(RpcError::new(6, "Queue entry not found"));
                    }
                    self.state.current = Some(id);
                    self.start_current(0.0).await?;
                }
                AppCommand::Mode { mode } => self.state.mode = mode,
                AppCommand::QueueMove { id, to } => {
                    let from = self
                        .state
                        .queue
                        .iter()
                        .position(|entry| entry.id == id)
                        .ok_or_else(|| RpcError::new(6, "Queue entry not found"))?;
                    if to >= self.state.queue.len() {
                        return Err(RpcError::new(2, "Queue index out of range"));
                    }
                    let entry = self.state.queue.remove(from);
                    self.state.queue.insert(to, entry);
                    queue_changed = true;
                }
                AppCommand::Pause => {
                    if self.loading.is_some() {
                        self.stop_backend().await;
                    }
                    self.stop_outgoing().await;
                    if self.state.current.is_none() {
                        return Err(RpcError::new(6, "Nothing to pause"));
                    }
                    if let Some(backend) = &mut self.backend {
                        backend.pause(true).await.map_err(audio_error)?;
                    }
                    self.desired_pause = true;
                    self.state.state = PlaybackState::Paused;
                    if let Some(pending) = self.pending.take() {
                        let _ = pending.reply.send(Ok(self.state.clone()));
                    }
                }
                AppCommand::Resume => {
                    if self.state.current.is_none() {
                        let id = self
                            .state
                            .queue
                            .first()
                            .ok_or_else(|| RpcError::new(6, "Queue is empty"))?
                            .id;
                        self.state.current = Some(id);
                    }
                    if let Some(backend) = &mut self.backend {
                        backend.pause(false).await.map_err(audio_error)?;
                        self.desired_pause = false;
                        self.state.state = PlaybackState::Playing;
                        self.last_position = Instant::now();
                    } else {
                        let position = if self.state.state == PlaybackState::Ended {
                            0.0
                        } else {
                            self.state.position
                        };
                        self.start_current(position).await?;
                    }
                }
                AppCommand::Seek { seconds } => {
                    if !seconds.is_finite() || seconds < 0.0 {
                        return Err(RpcError::new(
                            2,
                            "Seek seconds must be finite and nonnegative",
                        ));
                    }
                    if self.state.current.is_none() {
                        return Err(RpcError::new(6, "Nothing to seek"));
                    }
                    let seconds = self
                        .state
                        .duration
                        .map_or(seconds, |duration| seconds.min(duration));
                    if let Some(backend) = &mut self.backend {
                        backend.seek(seconds).await.map_err(audio_error)?;
                    }
                    if self.loading.is_some() {
                        self.start_current(seconds).await?;
                    }
                    self.state.position = seconds;
                    self.last_position = Instant::now();
                }
                AppCommand::Volume { percent } => {
                    if percent > 100 {
                        return Err(RpcError::new(2, "Volume must be between 0 and 100"));
                    }
                    if let Some(backend) = &mut self.backend {
                        backend.volume(percent).await.map_err(audio_error)?;
                    }
                    self.state.volume = percent;
                }
                AppCommand::Next | AppCommand::Previous => {
                    let index = self
                        .state
                        .current
                        .and_then(|id| self.state.queue.iter().position(|entry| entry.id == id));
                    let next = if previous {
                        index.and_then(|index| index.checked_sub(1))
                    } else if self.state.mode == PlaybackMode::Shuffle
                        && !self.state.queue.is_empty()
                    {
                        let length = self.state.queue.len();
                        let mut selected = rand::random_range(0..length);
                        if length > 1 && Some(selected) == index {
                            selected = (selected + 1) % length;
                        }
                        Some(selected)
                    } else {
                        Some(index.map_or(0, |index| index + 1))
                            .filter(|index| *index < self.state.queue.len())
                    };
                    let next =
                        next.ok_or_else(|| RpcError::new(6, "No queue entry in that direction"))?;
                    self.state.current = Some(self.state.queue[next].id);
                    self.start_current(0.0).await?;
                }
                AppCommand::Stop | AppCommand::QueueClear => {
                    self.stop_backend().await;
                    self.state.state = PlaybackState::Idle;
                    self.state.current = None;
                    self.state.position = 0.0;
                    self.state.duration = None;
                    self.state.error = None;
                    if clear {
                        self.state.queue.clear();
                        queue_changed = true;
                    }
                }
                AppCommand::QueueRemove { id } => {
                    let index = self
                        .state
                        .queue
                        .iter()
                        .position(|entry| entry.id == id)
                        .ok_or_else(|| RpcError::new(6, "Queue entry not found"))?;
                    if self.state.current == Some(id) {
                        self.stop_backend().await;
                        self.state.current = None;
                        self.state.position = 0.0;
                        self.state.duration = None;
                        self.state.state = PlaybackState::Idle;
                        self.state.error = None;
                    }
                    self.state.queue.remove(index);
                    queue_changed = true;
                }
                AppCommand::Shutdown => {
                    self.stop_backend().await;
                    self.desired_pause = true;
                    self.state.state = if self.state.current.is_some() {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Idle
                    };
                }
                AppCommand::Snapshot
                | AppCommand::Subscribe
                | AppCommand::Service { .. }
                | AppCommand::Detached { .. } => {
                    unreachable!()
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = &action
            && (3..=5).contains(&error.code)
        {
            self.stop_backend().await;
            self.state.state = PlaybackState::Error;
            self.state.error = Some(error.message.clone());
        }
        // Save queue edits transactionally, even when a new entry cannot be decoded.
        let saved = if queue_changed {
            self.storage.save(&self.state).await
        } else {
            self.storage.checkpoint(&self.state).await
        };
        if let Err(error) = saved {
            self.stop_backend().await;
            let sequence = self.state.sequence;
            self.state = before;
            self.state.sequence = sequence;
            self.state.state = PlaybackState::Error;
            self.state.error = Some(format!("Cannot save playback state: {error}"));
            self.publish();
            return Err(RpcError::new(
                1,
                self.state.error.clone().unwrap_or_default(),
            ));
        }
        self.publish();
        action?;
        Ok(self.state.clone())
    }

    async fn backend_event(&mut self, event: BackendEvent) {
        if self.loading.is_some() {
            match event {
                BackendEvent::Duration(duration) => self.state.duration = Some(duration),
                BackendEvent::Position(position) => self.state.position = position,
                BackendEvent::Buffering(true) => {
                    self.state.state = PlaybackState::Buffering;
                    self.publish();
                }
                _ => {}
            }
            return;
        }

        match event {
            BackendEvent::Position(position) => {
                self.state.position = position;
                self.last_position = Instant::now();
            }
            BackendEvent::Duration(duration) => self.state.duration = Some(duration),
            BackendEvent::Loaded => {
                self.state.state = if self.desired_pause {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                }
            }
            BackendEvent::Pause(paused) => {
                // Only the core changes the desired pause state. Ignore older acknowledgements.
                if paused == self.desired_pause && self.state.state != PlaybackState::Error {
                    self.state.state = if paused {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Playing
                    };
                    self.last_position = Instant::now();
                }
            }
            BackendEvent::Buffering(buffering)
                if !self.desired_pause
                    && matches!(
                        self.state.state,
                        PlaybackState::Playing | PlaybackState::Buffering
                    ) =>
            {
                self.advance_position();
                self.state.state = if buffering {
                    PlaybackState::Buffering
                } else {
                    PlaybackState::Playing
                };
            }
            BackendEvent::Buffering(_) => {}
            BackendEvent::Ended => {
                self.state.state = PlaybackState::Ended;
                let current = self
                    .state
                    .current
                    .and_then(|id| self.state.queue.iter().position(|e| e.id == id));
                let next = match self.state.mode {
                    PlaybackMode::RepeatOne => current,
                    PlaybackMode::Shuffle if !self.state.queue.is_empty() => {
                        let len = self.state.queue.len();
                        let mut index = rand::random_range(0..len);
                        if len > 1 && Some(index) == current {
                            index = (index + 1) % len;
                        }
                        Some(index)
                    }
                    _ => current
                        .map(|index| index + 1)
                        .filter(|index| *index < self.state.queue.len()),
                };
                self.stop_backend().await;
                if let Some(next) = next {
                    self.state.current = Some(self.state.queue[next].id);
                    let _ = self.start_current(0.0).await;
                }
                if let Err(error) = self.storage.checkpoint(&self.state).await {
                    self.stop_backend().await;
                    self.state.state = PlaybackState::Error;
                    self.state.error = Some(format!("Cannot save playback state: {error}"));
                }
            }
            BackendEvent::Failed(error) => {
                self.advance_position();
                self.stop_backend().await;
                self.state.state = PlaybackState::Error;
                self.state.error = Some(error);
            }
        }
        self.publish();
    }
}

fn audio_error(error: anyhow::Error) -> RpcError {
    RpcError::new(5, error.to_string())
}
