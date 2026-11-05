use crate::{
    domain::{Platform, Track},
    protocol::AppCommand,
    services::ServiceCommand,
};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageFormat, ImageReader, Limits, imageops::FilterType};
use ratatui::layout::Rect;
use std::{
    collections::VecDeque,
    io::{self, Cursor, Write},
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub type Image = Option<Arc<Vec<u8>>>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Preview,
    Playing,
}
impl Slot {
    pub fn request_id(self) -> usize {
        match self {
            Self::Preview => 97,
            Self::Playing => 98,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Track,
    Playlist,
}

#[derive(Clone)]
enum Source {
    Track(Track),
    Url(String),
    Library(Uuid),
    Missing,
}

#[derive(Clone)]
pub struct Target {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub kind: Kind,
    source: Source,
}
impl Target {
    pub fn track(track: Track) -> Self {
        Self {
            key: track
                .artwork
                .as_ref()
                .filter(|url| !url.is_empty())
                .map_or_else(|| format!("track:{}", track.id), |url| format!("url:{url}")),
            title: track.title.clone(),
            subtitle: track.artist.clone(),
            kind: Kind::Track,
            source: Source::Track(track),
        }
    }
    pub fn playlist(id: &str, title: String, subtitle: String, artwork: Option<String>) -> Self {
        let artwork = artwork.filter(|url| !url.is_empty());
        Self {
            key: artwork
                .as_ref()
                .map_or_else(|| format!("playlist:{id}"), |url| format!("url:{url}")),
            title,
            subtitle,
            kind: Kind::Playlist,
            source: artwork.map_or(Source::Missing, Source::Url),
        }
    }
    pub fn library(id: Uuid, title: String, count: u64) -> Self {
        Self {
            key: format!("library:{id}:{count}"),
            title,
            subtitle: String::new(),
            kind: Kind::Playlist,
            source: Source::Library(id),
        }
    }
}

#[derive(Default)]
pub struct State {
    pub target: Option<Target>,
    pub png: Image,
    pub area: Option<Rect>,
    due: Option<Instant>,
    requested: bool,
}
impl State {
    pub fn set(&mut self, target: Option<Target>, delay: Duration) -> bool {
        if self.target.as_ref().map(|target| &target.key)
            == target.as_ref().map(|target| &target.key)
        {
            let changed = self
                .target
                .as_ref()
                .map(|t| (&t.title, &t.subtitle, t.kind))
                != target.as_ref().map(|t| (&t.title, &t.subtitle, t.kind));
            self.target = target;
            return changed;
        }
        self.target = target;
        self.png = None;
        self.requested = false;
        self.due = Some(Instant::now() + delay);
        true
    }
    pub fn request(&mut self) -> Option<Target> {
        if self.requested || self.due.is_none_or(|due| Instant::now() < due) {
            return None;
        }
        let target = self.target.clone()?;
        self.requested = true;
        Some(target)
    }
    pub fn retry(&mut self) {
        self.requested = false;
    }
    pub fn apply(&mut self, key: &str, png: Image) -> bool {
        if self.target.as_ref().is_none_or(|target| target.key != key) {
            return false;
        }
        self.png = png;
        true
    }
    pub fn key(&self) -> Option<&str> {
        self.target.as_ref().map(|target| target.key.as_str())
    }
    pub fn pixels(&self) -> Option<&[u8]> {
        self.png.as_ref().map(|png| png.as_slice())
    }
}

struct Cached {
    key: String,
    png: Image,
    expires: Instant,
}
#[derive(Default)]
pub struct Cache {
    entries: VecDeque<Cached>,
    bytes: usize,
}
impl Cache {
    pub fn get(&mut self, key: &str) -> Option<Image> {
        let index = self.entries.iter().position(|entry| entry.key == key)?;
        let entry = self.entries.remove(index)?;
        if entry.expires <= Instant::now() {
            self.bytes -= entry.png.as_ref().map_or(0, |png| png.len());
            return None;
        }
        let png = entry.png.clone();
        self.entries.push_back(entry);
        Some(png)
    }
    pub fn insert(&mut self, key: String, png: Image) {
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key)
            && let Some(old) = self.entries.remove(index)
        {
            self.bytes -= old.png.as_ref().map_or(0, |png| png.len());
        }
        self.bytes += png.as_ref().map_or(0, |png| png.len());
        let ttl = if png.is_some() {
            Duration::from_secs(600)
        } else {
            Duration::from_secs(15)
        };
        self.entries.push_back(Cached {
            key,
            png,
            expires: Instant::now() + ttl,
        });
        while self.entries.len() > 32 || self.bytes > 16 * 1024 * 1024 {
            if let Some(old) = self.entries.pop_front() {
                self.bytes -= old.png.as_ref().map_or(0, |png| png.len());
            }
        }
    }
}

pub fn supported() -> bool {
    if std::env::var_os("TMUX").is_some() || std::env::var_os("STY").is_some() {
        return false;
    }
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    let program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_lowercase();
    term.contains("ghostty")
        || term.contains("kitty")
        || program.contains("ghostty")
        || program.contains("wezterm")
        || std::env::var_os("WEZTERM_EXECUTABLE").is_some()
}

pub async fn fetch(target: Target) -> Result<Vec<u8>> {
    let url = match target.source {
        Source::Url(url) => url,
        Source::Track(track) => track_url(track).await?,
        Source::Library(id) => {
            let value = crate::daemon::call_value(
                AppCommand::Service {
                    command: ServiceCommand::LibraryShow { id, page: 1 },
                },
                false,
            )
            .await?;
            let tracks: Vec<Track> = serde_json::from_value(value["items"].clone())?;
            let track = tracks
                .iter()
                .find(|track| track.artwork.as_ref().is_some_and(|url| !url.is_empty()))
                .or_else(|| tracks.first())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Empty playlist"))?;
            track_url(track).await?
        }
        Source::Missing => anyhow::bail!("No artwork"),
    };
    fetch_url(&url).await
}

async fn track_url(track: Track) -> Result<String> {
    let url = match track.artwork.filter(|s| !s.is_empty()) {
        Some(url) => url,
        None if track.platform == Some(Platform::Wy) => crate::catalog::Catalog::new()?
            .detail(&track.id)
            .await?
            .artwork
            .ok_or_else(|| anyhow::anyhow!("No artwork"))?,
        None => anyhow::bail!("No artwork"),
    };
    Ok(url)
}

async fn fetch_url(url: &str) -> Result<Vec<u8>> {
    let url = crate::http::url(url)?;
    let response = crate::http::client()?.get(url).send().await?;
    let bytes = crate::http::bytes(response, 8 * 1024 * 1024).await?;
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode()?.resize(256, 256, FilterType::Triangle);
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, ImageFormat::Png)?;
        let png = png.into_inner();
        ensure!(png.len() <= 1024 * 1024, "Artwork is too large");
        Ok(png)
    })
    .await?
}

pub struct Renderer {
    id: u32,
    shown: Option<(String, Rect)>,
}
impl Renderer {
    pub fn new(slot: Slot) -> Self {
        Self {
            id: std::process::id()
                .wrapping_mul(2)
                .wrapping_add(if slot == Slot::Playing { 1 } else { 0 })
                .max(1),
            shown: None,
        }
    }
    pub fn draw(
        &mut self,
        area: Option<Rect>,
        key: Option<&str>,
        png: Option<&[u8]>,
    ) -> io::Result<()> {
        let next = area
            .zip(key)
            .zip(png)
            .map(|((area, key), png)| (area, key, png));
        if self.shown.as_ref().is_some_and(|(old, rect)| {
            next.as_ref()
                .is_some_and(|(area, key, _)| old == key && rect == area)
        }) {
            return Ok(());
        }
        let mut output = io::stdout().lock();
        if self.shown.take().is_some() {
            write!(output, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", self.id)?;
        }
        if let Some((area, key, png)) = next {
            let encoded = STANDARD.encode(png);
            write!(output, "\x1b7\x1b[{};{}H", area.y + 1, area.x + 1)?;
            let chunks = encoded.as_bytes().chunks(4096);
            let count = chunks.len();
            for (index, chunk) in chunks.enumerate() {
                if index == 0 {
                    write!(
                        output,
                        "\x1b_Ga=T,f=100,t=d,i={},q=2,c={},r={},C=1,m={};",
                        self.id,
                        area.width,
                        area.height,
                        u8::from(count > 1)
                    )?;
                } else {
                    write!(output, "\x1b_Gm={};", u8::from(index + 1 < count))?;
                }
                output.write_all(chunk)?;
                write!(output, "\x1b\\")?;
            }
            write!(output, "\x1b8")?;
            self.shown = Some((key.to_owned(), area));
        }
        output.flush()
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        if self.shown.is_some() {
            let _ = write!(io::stdout(), "\x1b_Ga=d,d=I,i={},q=2\x1b\\", self.id);
            let _ = io::stdout().flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(id: &str) -> Target {
        Target::playlist(
            id,
            id.into(),
            String::new(),
            Some(format!("https://example.com/{id}.png")),
        )
    }

    #[test]
    fn selection_debounces_and_stale_preview_cannot_change_playing_artwork() {
        let mut preview = State::default();
        let mut playing = State::default();
        let a = target("a");
        let b = target("b");
        let png = Some(Arc::new(vec![1, 2, 3]));
        playing.set(Some(a.clone()), Duration::ZERO);
        assert!(playing.request().is_some());
        assert!(playing.apply(&a.key, png.clone()));
        preview.set(Some(a.clone()), Duration::from_millis(200));
        assert!(preview.request().is_none());
        preview.set(Some(b.clone()), Duration::ZERO);
        assert_eq!(preview.request().unwrap().key, b.key);
        assert!(preview.request().is_none());
        assert!(!preview.apply(&a.key, png.clone()));
        assert!(preview.png.is_none());
        assert!(preview.apply(&b.key, None));
        assert_eq!(playing.key(), Some(a.key.as_str()));
        assert_eq!(playing.png, png);
    }

    #[test]
    fn cache_reuses_images_and_evicts_least_recently_used() {
        let mut cache = Cache::default();
        let png = Some(Arc::new(vec![1, 2, 3]));
        cache.insert("a".into(), png.clone());
        cache.insert("b".into(), None);
        assert_eq!(cache.get("a"), Some(png));
        assert_eq!(cache.get("b"), Some(None));
        for n in 0..32 {
            cache.insert(format!("new{n}"), None);
        }
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_none());
        assert!(cache.get("new31").is_some());
    }
}
