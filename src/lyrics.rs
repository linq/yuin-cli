use crate::{
    domain::{Platform, Track},
    paths::AppPaths,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{io::Read, path::PathBuf, sync::LazyLock};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Line {
    pub time: f64,
    pub text: String,
    pub translation: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    pub plain: String,
}
impl Lyrics {
    pub fn active(&self, position: f64) -> Option<usize> {
        if !position.is_finite() {
            return None;
        }
        let index = self.lines.partition_point(|line| line.time <= position);
        index.checked_sub(1)
    }
    pub fn parse(text: &str, translation: &str) -> Self {
        static TIME: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]").expect("timestamp regex")
        });
        static KRC: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"^\[(\d+),\d+\]").expect("KRC regex"));
        static WORD: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"<\d+,\d+(?:,\d+)?>|\(\d+,\d+(?:,\d+)?\)").expect("word regex")
        });
        static OFFSET: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"(?i)\[offset:([+-]?\d+)\]").expect("offset regex")
        });
        static META: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"^\[[a-zA-Z]+:.*\]$").expect("metadata regex"));
        fn lines(value: &str) -> Lyrics {
            if value.len() > 2_000_000
                || value.trim_start().to_lowercase().starts_with("<html")
                || value.trim_start().to_lowercase().starts_with("<!doctype")
            {
                return Lyrics {
                    lines: Vec::new(),
                    plain: String::new(),
                };
            }
            let offset = OFFSET
                .captures(value)
                .and_then(|c| c[1].parse::<f64>().ok())
                .unwrap_or_default()
                / 1000.0;
            let mut lines = Vec::new();
            let mut plain = Vec::new();
            for raw in value.lines().take(5000) {
                let raw = raw.trim();
                let text = TIME.replace_all(raw, "");
                let text = KRC.replace(&text, "");
                let text = WORD
                    .replace_all(&text, "")
                    .trim()
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect::<String>();
                let mut times = TIME
                    .captures_iter(raw)
                    .filter_map(|c| {
                        let minute = c[1].parse::<f64>().ok()?;
                        let second = c[2].parse::<f64>().ok()?;
                        if second >= 60.0 {
                            return None;
                        }
                        let fraction = c
                            .get(3)
                            .and_then(|m| format!("0.{}", m.as_str()).parse::<f64>().ok())
                            .unwrap_or_default();
                        Some((minute * 60.0 + second + fraction - offset).max(0.0))
                    })
                    .collect::<Vec<_>>();
                if let Some(c) = KRC.captures(raw) {
                    times
                        .push((c[1].parse::<f64>().unwrap_or_default() / 1000.0 - offset).max(0.0));
                }
                if times.is_empty() {
                    if !raw.is_empty() && !META.is_match(raw) && !raw.starts_with('{') {
                        plain.push(text);
                    }
                } else {
                    for time in times {
                        lines.push(Line {
                            time,
                            text: text.clone(),
                            translation: None,
                        });
                    }
                }
            }
            lines.sort_by(|a, b| a.time.total_cmp(&b.time));
            let mut merged: Vec<Line> = Vec::new();
            for line in lines {
                if let Some(last) = merged.last_mut()
                    && (last.time - line.time).abs() < 0.001
                {
                    if !line.text.is_empty() && !last.text.lines().any(|s| s == line.text) {
                        if !last.text.is_empty() {
                            last.text.push('\n');
                        }
                        last.text.push_str(&line.text);
                    }
                } else {
                    merged.push(line);
                }
            }
            Lyrics {
                lines: merged,
                plain: plain.join("\n"),
            }
        }
        let mut main = lines(text);
        let translated = lines(translation);
        for line in &mut main.lines {
            if let Some(other) = translated
                .lines
                .iter()
                .find(|other| (other.time - line.time).abs() <= 0.1)
                && other.text != line.text
            {
                line.translation = Some(other.text.clone());
            }
        }
        main
    }
}

pub struct LyricsService {
    client: reqwest::Client,
    cache: PathBuf,
}
impl LyricsService {
    pub fn new(paths: &AppPaths) -> Result<Self> {
        Ok(Self {
            client: crate::http::client()?,
            cache: paths.cache.join("lyrics"),
        })
    }
    pub async fn get(&self, track: &Track, refresh: bool) -> Result<Lyrics> {
        let hash = Sha256::digest(track.id.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let path = self.cache.join(format!("{hash}.json"));
        if !refresh
            && let Ok(meta) = tokio::fs::metadata(&path).await
            && meta.len() <= 2_000_000
            && let Ok(data) = tokio::fs::read(&path).await
            && let Ok(cached) = serde_json::from_slice(&data)
        {
            return Ok(cached);
        }
        let document = if track.platform.is_none() {
            let file = PathBuf::from(&track.path).with_extension("lrc");
            ensure!(
                tokio::fs::metadata(&file).await?.len() <= 2_000_000,
                "Lyric file is too large"
            );
            Lyrics::parse(&tokio::fs::read_to_string(file).await?, "")
        } else {
            self.fetch(track).await?
        };
        if !document.lines.is_empty() || !document.plain.is_empty() {
            let bytes = serde_json::to_vec(&document)?;
            tokio::task::spawn_blocking(move || crate::config::atomic_write(&path, &bytes))
                .await??;
        }
        Ok(document)
    }
    async fn request(&self, endpoint: &str, params: &[(&str, String)]) -> Result<Vec<u8>> {
        let mut url = crate::http::url(endpoint)?;
        if let Ok(base) = std::env::var("YUIN_CATALOG_BASE") {
            url = crate::http::url(&base)?.join(url.path())?;
        }
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v)));
        crate::http::bytes(
            self.client
                .get(url)
                .header("Referer", endpoint)
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Lyrics network request failed"))?,
            2_000_000,
        )
        .await
    }
    async fn object(&self, url: &str, params: &[(&str, String)]) -> Result<Value> {
        serde_json::from_slice(&self.request(url, params).await?).context("Invalid lyrics response")
    }
    async fn fetch(&self, track: &Track) -> Result<Lyrics> {
        match track.platform.context("Missing platform")? {
            Platform::Wy => {
                let d = self
                    .object(
                        "https://music.163.com/api/song/lyric",
                        &[
                            ("id", track.song_id.clone()),
                            ("lv", "-1".into()),
                            ("kv", "-1".into()),
                            ("tv", "-1".into()),
                        ],
                    )
                    .await?;
                ensure!(d["code"] == 200, "NetEase lyrics unavailable");
                Ok(Lyrics::parse(
                    d["lrc"]["lyric"].as_str().unwrap_or_default(),
                    d["tlyric"]["lyric"].as_str().unwrap_or_default(),
                ))
            }
            Platform::Tx => {
                let d = self
                    .object(
                        "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg",
                        &[
                            ("songmid", track.song_id.clone()),
                            ("g_tk", "5381".into()),
                            ("format", "json".into()),
                            ("nobase64", "1".into()),
                            ("platform", "yqq.json".into()),
                        ],
                    )
                    .await?;
                ensure!(d["code"] == 0, "QQ lyrics unavailable");
                let decode = |key: &str| {
                    let text = d[key].as_str().unwrap_or_default();
                    if text.contains('[') {
                        text.to_string()
                    } else {
                        STANDARD
                            .decode(text)
                            .ok()
                            .map(|b| String::from_utf8_lossy(&b).into_owned())
                            .unwrap_or_else(|| text.into())
                    }
                };
                Ok(Lyrics::parse(&decode("lyric"), &decode("trans")))
            }
            Platform::Kw => {
                let data = self
                    .request(
                        "https://mlyric.kuwo.cn/mobi.s",
                        &[
                            ("f", "web".into()),
                            ("type", "lyric".into()),
                            ("lrcx", "0".into()),
                            ("rid", track.song_id.clone()),
                            ("encode", "utf8".into()),
                        ],
                    )
                    .await?;
                ensure!(data.starts_with(b"tp=content"), "Kuwo lyrics unavailable");
                let end = data
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .context("Invalid Kuwo lyric header")?;
                let mut text = inflate(&data[end + 4..])?;
                if String::from_utf8_lossy(&data[..end])
                    .to_lowercase()
                    .contains("lrcx=1")
                {
                    let decoded = STANDARD.decode(text.trim())?;
                    text = String::from_utf8_lossy(
                        &decoded
                            .iter()
                            .enumerate()
                            .map(|(i, b)| b ^ b"yeelion"[i % 7])
                            .collect::<Vec<_>>(),
                    )
                    .into_owned();
                }
                Ok(Lyrics::parse(&text, ""))
            }
            Platform::Kg => {
                let hash = track.source_info["hash"]
                    .as_str()
                    .context("Kugou track has no hash")?;
                let search = self
                    .object(
                        "https://lyrics.kugou.com/search",
                        &[
                            ("ver", "1".into()),
                            ("man", "yes".into()),
                            ("client", "pc".into()),
                            ("hash", hash.into()),
                            ("timelength", ((track.duration * 1000.0) as u64).to_string()),
                            ("lrctxt", "1".into()),
                        ],
                    )
                    .await?;
                let candidate = &search["candidates"][0];
                ensure!(candidate.is_object(), "Kugou lyric not found");
                let id = candidate["id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| candidate["id"].to_string());
                let d = self
                    .object(
                        "https://lyrics.kugou.com/download",
                        &[
                            ("ver", "1".into()),
                            ("client", "pc".into()),
                            ("id", id),
                            (
                                "accesskey",
                                candidate["accesskey"].as_str().unwrap_or_default().into(),
                            ),
                            ("fmt", "lrc".into()),
                            ("charset", "utf8".into()),
                        ],
                    )
                    .await?;
                let data =
                    STANDARD.decode(d["content"].as_str().context("Missing Kugou lyrics")?)?;
                let text = if data.starts_with(b"krc1") {
                    let key = [
                        0x40, 0x47, 0x61, 0x77, 0x5e, 0x32, 0x74, 0x47, 0x51, 0x36, 0x31, 0x2d,
                        0xce, 0xd2, 0x6e, 0x69,
                    ];
                    inflate(
                        &data[4..]
                            .iter()
                            .enumerate()
                            .map(|(i, b)| b ^ key[i % key.len()])
                            .collect::<Vec<_>>(),
                    )?
                } else {
                    String::from_utf8(data)?
                };
                Ok(Lyrics::parse(&text, ""))
            }
            Platform::Mg => {
                let mut url = track.source_info["lrcUrl"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let mut translation = track.source_info["trcUrl"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if url.is_empty() {
                    let d = self
                        .object(
                            "https://c.musicapp.migu.cn/MIGUM2.0/v1.0/content/resourceinfo.do",
                            &[
                                ("resourceType", "2".into()),
                                ("resourceId", track.song_id.clone()),
                            ],
                        )
                        .await?;
                    let root = if d["data"].is_object() {
                        &d["data"]
                    } else {
                        &d
                    };
                    let r = &root["resource"][0];
                    url = r["lrcUrl"].as_str().unwrap_or_default().into();
                    translation = r["trcUrl"].as_str().unwrap_or_default().into();
                }
                ensure!(!url.is_empty(), "Migu lyrics not found");
                let main = String::from_utf8(self.request(&url, &[]).await?)?;
                let translated = if translation.is_empty() {
                    String::new()
                } else {
                    self.request(&translation, &[])
                        .await
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default()
                };
                Ok(Lyrics::parse(&main, &translated))
            }
        }
    }
}
fn inflate(bytes: &[u8]) -> Result<String> {
    let mut output = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .take(2_000_001)
        .read_to_end(&mut output)?;
    ensure!(
        output.len() <= 2_000_000,
        "Lyrics decompression limit exceeded"
    );
    String::from_utf8(output).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamps_offsets_repeats_translation_and_binary_search() {
        let lyrics = Lyrics::parse(
            "[offset:100]\n[00:01.20][00:03]你好\n[00:01.200]世界\n[00:05]下一句",
            "[00:01.1]hello",
        );
        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].text, "你好\n世界");
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("hello"));
        assert_eq!(lyrics.active(0.0), None);
        assert_eq!(lyrics.active(1.1), Some(0));
        assert_eq!(lyrics.active(3.0), Some(1));
        assert_eq!(lyrics.active(f64::NAN), None);
    }
    #[test]
    fn krc_word_timing_and_html_rejection() {
        let doc = Lyrics::parse("[1000,2000]<0,500,0>hello<500,500,0> world", "");
        assert_eq!(doc.lines[0].text, "hello world");
        assert!(Lyrics::parse("<html>error</html>", "").lines.is_empty());
    }
}
