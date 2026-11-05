use crate::domain::{Platform, Track};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageFormat, ImageReader, Limits, imageops::FilterType};
use ratatui::layout::Rect;
use std::io::{self, Cursor, Write};

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

pub async fn fetch(track: Track) -> Result<Vec<u8>> {
    let url = match track.artwork.filter(|s| !s.is_empty()) {
        Some(url) => url,
        None if track.platform == Some(Platform::Wy) => crate::catalog::Catalog::new()?
            .detail(&track.id)
            .await?
            .artwork
            .ok_or_else(|| anyhow::anyhow!("No artwork"))?,
        None => anyhow::bail!("No artwork"),
    };
    let url = crate::http::url(&url)?;
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
    pub fn new() -> Self {
        Self {
            id: std::process::id().max(1),
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
