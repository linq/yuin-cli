use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::{mpsc, oneshot};

use crate::domain::{PlaybackState, QueueEntry, Snapshot};

enum Operation {
    Run(Box<dyn FnOnce(&mut Connection) + Send>),
    Load(oneshot::Sender<Result<Snapshot>>),
    Save(Snapshot, oneshot::Sender<Result<()>>),
    Checkpoint(Snapshot, oneshot::Sender<Result<()>>),
}

#[derive(Clone)]
pub struct Storage {
    path: PathBuf,
    sender: mpsc::Sender<Operation>,
}

impl Storage {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let db_path = path.clone();
        let mut db = tokio::task::spawn_blocking(move || open(db_path)).await??;
        let (sender, mut receiver) = mpsc::channel(8);
        // The connection and every transaction live on this dedicated blocking thread.
        std::thread::Builder::new()
            .name("yuin-sqlite".into())
            .spawn(move || {
                while let Some(op) = receiver.blocking_recv() {
                    match op {
                        Operation::Run(run) => run(&mut db),
                        Operation::Load(reply) => {
                            let _ = reply.send(load(&db));
                        }
                        Operation::Save(state, reply) => {
                            let _ = reply.send(save(&mut db, &state));
                        }
                        Operation::Checkpoint(state, reply) => {
                            let _ = reply.send(checkpoint(&db, &state));
                        }
                    }
                }
            })?;
        Ok(Self { sender, path })
    }

    pub async fn load(&self) -> Result<Snapshot> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(Operation::Load(tx))
            .await
            .map_err(|_| anyhow::anyhow!("SQLite worker stopped"))?;
        rx.await?
    }

    pub async fn save(&self, state: &Snapshot) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(Operation::Save(state.clone(), tx))
            .await
            .map_err(|_| anyhow::anyhow!("SQLite worker stopped"))?;
        rx.await?
    }

    pub async fn checkpoint(&self, state: &Snapshot) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(Operation::Checkpoint(state.clone(), tx))
            .await
            .map_err(|_| anyhow::anyhow!("SQLite worker stopped"))?;
        rx.await?
    }

    async fn execute<T: Send + 'static>(
        &self,
        run: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(Operation::Run(Box::new(move |db| {
                let _ = tx.send(run(db));
            })))
            .await
            .map_err(|_| anyhow::anyhow!("SQLite worker stopped"))?;
        rx.await?
    }

    pub async fn backup(&self) -> Result<PathBuf> {
        let path = self
            .path
            .with_extension(format!("sqlite3.backup-{}", uuid::Uuid::new_v4()));
        self.execute(move |db| {
            db.backup("main", &path, rusqlite::backup::NO_PROGRESS)?;
            Ok(path)
        })
        .await
    }

    pub async fn import_records(
        &self,
        lists: Vec<crate::domain::LocalPlaylist>,
        sources: Vec<serde_json::Value>,
        downloads: Vec<serde_json::Value>,
        history: Vec<crate::domain::Track>,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            for list in lists {
                for track in &list.tracks {
                    cache_track(&tx, track)?;
                }
                tx.execute(
                    "INSERT INTO playlists(id,metadata) VALUES(?1,?2)",
                    params![list.id.to_string(), serde_json::to_string(&list)?],
                )?;
            }
            for value in sources {
                tx.execute(
                    "INSERT INTO sources(id,metadata) VALUES(?1,?2)",
                    params![
                        value["id"].as_str().context("Source has no ID")?,
                        serde_json::to_string(&value)?
                    ],
                )?;
            }
            for value in downloads {
                let track: crate::domain::Track = serde_json::from_value(value["track"].clone())?;
                cache_track(&tx, &track)?;
                tx.execute(
                    "INSERT INTO downloads(id,metadata) VALUES(?1,?2)",
                    params![
                        value["id"].as_str().context("Download has no ID")?,
                        serde_json::to_string(&value)?
                    ],
                )?;
            }
            for track in history.into_iter().take(1000) {
                cache_track(&tx, &track)?;
                tx.execute("INSERT INTO history(track_id) VALUES(?1)", [track.id])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn put_tracks(&self, tracks: Vec<crate::domain::Track>) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            for track in tracks {
                cache_track(&tx, &track)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn track(&self, id: String) -> Result<Option<crate::domain::Track>> {
        self.execute(move |db| {
            let text = db
                .query_row("SELECT metadata FROM tracks WHERE id=?1", [id], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?;
            text.map(|s| serde_json::from_str(&s).map_err(Into::into))
                .transpose()
        })
        .await
    }

    pub async fn offline_records(
        &self,
        id: String,
        quality: String,
    ) -> Result<Vec<serde_json::Value>> {
        self.execute(move|db|{
            let mut statement=db.prepare("SELECT metadata FROM downloads WHERE json_extract(metadata,'$.track.id')=?1 AND json_extract(metadata,'$.quality')=?2 AND json_extract(metadata,'$.state')='completed' ORDER BY rowid DESC")?;
            let rows=statement.query_map(params![id,quality],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;rows.iter().map(|s|serde_json::from_str(s).map_err(Into::into)).collect()
        }).await
    }

    pub async fn record_put(
        &self,
        kind: RecordKind,
        id: String,
        value: serde_json::Value,
    ) -> Result<()> {
        self.execute(move |db| { db.execute(&format!("INSERT INTO {}(id,metadata) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata",kind.table()),params![id,serde_json::to_string(&value)?])?; Ok(()) }).await
    }

    pub async fn record_get(
        &self,
        kind: RecordKind,
        id: String,
    ) -> Result<Option<serde_json::Value>> {
        self.execute(move |db| {
            let text = db
                .query_row(
                    &format!("SELECT metadata FROM {} WHERE id=?1", kind.table()),
                    [id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            text.map(|s| serde_json::from_str(&s).map_err(Into::into))
                .transpose()
        })
        .await
    }

    pub async fn record_list(&self, kind: RecordKind) -> Result<Vec<serde_json::Value>> {
        self.execute(move |db| {
            let mut statement = db.prepare(&format!(
                "SELECT metadata FROM {} ORDER BY rowid DESC",
                kind.table()
            ))?;
            let texts = statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            texts
                .iter()
                .map(|s| serde_json::from_str(s).map_err(Into::into))
                .collect()
        })
        .await
    }

    pub async fn record_delete(&self, kind: RecordKind, id: String) -> Result<bool> {
        self.execute(move |db| {
            Ok(db.execute(&format!("DELETE FROM {} WHERE id=?1", kind.table()), [id])? > 0)
        })
        .await
    }

    pub async fn library_edit(
        &self,
        id: uuid::Uuid,
        edit: LibraryEdit,
    ) -> Result<crate::domain::LocalPlaylist> {
        self.execute(move |db| {
            let tx=db.transaction()?;
            let text=tx.query_row("SELECT metadata FROM playlists WHERE id=?1",[id.to_string()],|r|r.get::<_,String>(0)).optional()?;
            let existed=text.is_some();
            let mut list:crate::domain::LocalPlaylist=if let Some(text)=text {serde_json::from_str(&text)?} else {ensure!(matches!(edit,LibraryEdit::Create{..}|LibraryEdit::CreateWithTracks{..}|LibraryEdit::CreateOnce{..}),"Library list not found");crate::domain::LocalPlaylist{id,name:String::new(),tracks:Vec::new()}};
            if existed && matches!(edit,LibraryEdit::CreateOnce{..}) {return Ok(list);}
            match edit {
                LibraryEdit::CreateWithTracks{name,tracks}|LibraryEdit::CreateOnce{name,tracks}=>{list.name=name;let mut seen=std::collections::HashSet::new();for track in tracks{cache_track(&tx,&track)?;if seen.insert(track.id.clone()){list.tracks.push(track);}}},
                LibraryEdit::Create{name} | LibraryEdit::Rename{name} => list.name=name,
                LibraryEdit::Add{tracks} => {let mut seen:std::collections::HashSet<_>=list.tracks.iter().map(|t|t.id.clone()).collect();for track in tracks {cache_track(&tx,&track)?;if seen.insert(track.id.clone()){list.tracks.push(track);}}},
                LibraryEdit::Remove{index} => {ensure!(index<list.tracks.len(),"Track index out of range");list.tracks.remove(index);},
                LibraryEdit::Move{from,to} => {ensure!(from<list.tracks.len() && to<list.tracks.len(),"Track index out of range");let track=list.tracks.remove(from);list.tracks.insert(to,track);},
            }
            ensure!(list.tracks.len()<=10000,"Personal list exceeds 10000 tracks");
            tx.execute("INSERT INTO playlists(id,metadata) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata",params![id.to_string(),serde_json::to_string(&list)?])?;
            tx.commit()?;
            Ok(list)
        }).await
    }

    pub async fn history_add(&self, track: crate::domain::Track) -> Result<()> {
        self.execute(move |db| {let tx=db.transaction()?;cache_track(&tx,&track)?;tx.execute("INSERT INTO history(track_id) VALUES(?1)",[track.id])?;tx.execute("DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY id DESC LIMIT 1000)",[])?;tx.commit()?;Ok(())}).await
    }

    pub async fn history(&self) -> Result<Vec<serde_json::Value>> {
        self.execute(move |db| {let mut stmt=db.prepare("SELECT h.played_at,t.metadata FROM history h JOIN tracks t ON t.id=h.track_id ORDER BY h.id DESC LIMIT 1000")?;let rows=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?;rows.map(|row|{let (played_at,text)=row?;Ok(serde_json::json!({"played_at":played_at,"track":serde_json::from_str::<serde_json::Value>(&text)?}))}).collect()}).await
    }
}

#[derive(Clone, Copy)]
pub enum RecordKind {
    Source,
    Download,
    Playlist,
}
impl RecordKind {
    fn table(self) -> &'static str {
        match self {
            Self::Source => "sources",
            Self::Download => "downloads",
            Self::Playlist => "playlists",
        }
    }
}
pub enum LibraryEdit {
    CreateOnce {
        name: String,
        tracks: Vec<crate::domain::Track>,
    },
    CreateWithTracks {
        name: String,
        tracks: Vec<crate::domain::Track>,
    },
    Create {
        name: String,
    },
    Rename {
        name: String,
    },
    Add {
        tracks: Vec<crate::domain::Track>,
    },
    Remove {
        index: usize,
    },
    Move {
        from: usize,
        to: usize,
    },
}

fn cache_track(db: &Connection, track: &crate::domain::Track) -> Result<()> {
    db.execute("INSERT INTO tracks(id,metadata) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata",params![track.id,serde_json::to_string(track)?])?;
    Ok(())
}

fn checkpoint(db: &Connection, state: &Snapshot) -> Result<()> {
    db.execute("INSERT INTO queue_state(id,current_entry,position,volume,mode) VALUES(1,?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET current_entry=excluded.current_entry, position=excluded.position, volume=excluded.volume, mode=excluded.mode", params![state.current.map(|id| id.to_string()), state.position, state.volume,state.mode.as_str()])?;
    Ok(())
}

fn open(path: PathBuf) -> Result<Connection> {
    let db = Connection::open(&path)?;
    db.busy_timeout(Duration::from_secs(2))?;
    db.execute_batch("PRAGMA foreign_keys=ON;")?;
    let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(version <= 3, "Database was created by a newer Yuin version");
    if version == 0 {
        // Even the initial migration backs up any existing database before changing it.
        let backup = path.with_extension("sqlite3.before-v1");
        if path.metadata()?.len() > 0 {
            ensure!(
                !backup.exists(),
                "Migration backup already exists: {}",
                backup.display()
            );
            db.backup("main", &backup, rusqlite::backup::NO_PROGRESS)?;
        }
        db.execute_batch("BEGIN IMMEDIATE;
            CREATE TABLE tracks (id TEXT PRIMARY KEY, metadata TEXT NOT NULL);
            CREATE TABLE queue_entries (id TEXT PRIMARY KEY, track_id TEXT NOT NULL REFERENCES tracks(id), ordinal INTEGER NOT NULL UNIQUE);
            CREATE TABLE queue_state (id INTEGER PRIMARY KEY CHECK(id=1), current_entry TEXT REFERENCES queue_entries(id), position REAL NOT NULL, volume INTEGER NOT NULL CHECK(volume BETWEEN 0 AND 100));
            CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
            INSERT INTO schema_migrations(version) VALUES(1);
            PRAGMA user_version=1;
            COMMIT;")?;
    }
    if version < 2 {
        if version > 0 {
            db.backup(
                "main",
                path.with_extension(format!("sqlite3.before-v2-{}", uuid::Uuid::new_v4())),
                rusqlite::backup::NO_PROGRESS,
            )?;
        }
        db.execute_batch("BEGIN IMMEDIATE;
            CREATE TABLE sources(id TEXT PRIMARY KEY, metadata TEXT NOT NULL);
            CREATE TABLE downloads(id TEXT PRIMARY KEY, metadata TEXT NOT NULL);
            CREATE TABLE playlists(id TEXT PRIMARY KEY, metadata TEXT NOT NULL);
            CREATE TABLE history(id INTEGER PRIMARY KEY, track_id TEXT NOT NULL REFERENCES tracks(id), played_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
            INSERT INTO schema_migrations(version) VALUES(2);
            PRAGMA user_version=2;
            COMMIT;")?;
    }
    if version < 3 {
        if version > 0 {
            db.backup(
                "main",
                path.with_extension(format!("sqlite3.before-v3-{}", uuid::Uuid::new_v4())),
                rusqlite::backup::NO_PROGRESS,
            )?;
        }
        db.execute_batch("BEGIN IMMEDIATE; ALTER TABLE queue_state ADD COLUMN mode TEXT NOT NULL DEFAULT 'sequential'; INSERT INTO schema_migrations(version) VALUES(3); PRAGMA user_version=3; COMMIT;")?;
    }
    Ok(db)
}

fn load(db: &Connection) -> Result<Snapshot> {
    let mut snapshot = Snapshot::default();
    let mut stmt = db.prepare("SELECT q.id, t.metadata FROM queue_entries q JOIN tracks t ON t.id=q.track_id ORDER BY q.ordinal")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, metadata) = row?;
        snapshot.queue.push(QueueEntry {
            id: id.parse()?,
            track: serde_json::from_str(&metadata)?,
        });
    }
    if let Some((current, position, volume, mode)) = db
        .query_row(
            "SELECT current_entry, position, volume,mode FROM queue_state WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, u8>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
    {
        snapshot.current = current.map(|value| value.parse()).transpose()?;
        snapshot.position = position;
        snapshot.volume = volume;
        snapshot.mode = serde_json::from_value(serde_json::json!(mode))?;
        if snapshot.current.is_some() {
            snapshot.state = PlaybackState::Paused;
        }
    }
    ensure!(
        snapshot.current.is_none() || snapshot.current_entry().is_some(),
        "Saved queue references a missing entry"
    );
    Ok(snapshot)
}

fn save(db: &mut Connection, state: &Snapshot) -> Result<()> {
    let tx = db.transaction().context("Cannot begin queue transaction")?;
    tx.execute("DELETE FROM queue_state", [])?;
    tx.execute("DELETE FROM queue_entries", [])?;
    for (ordinal, entry) in state.queue.iter().enumerate() {
        tx.execute("INSERT INTO tracks(id,metadata) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata", params![entry.track.id, serde_json::to_string(&entry.track)?])?;
        tx.execute(
            "INSERT INTO queue_entries(id,track_id,ordinal) VALUES(?1,?2,?3)",
            params![entry.id.to_string(), entry.track.id, ordinal as i64],
        )?;
    }
    tx.execute(
        "INSERT INTO queue_state(id,current_entry,position,volume,mode) VALUES(1,?1,?2,?3,?4)",
        params![
            state.current.map(|id| id.to_string()),
            state.position,
            state.volume,
            state.mode.as_str()
        ],
    )?;
    tx.commit()?;
    Ok(())
}
