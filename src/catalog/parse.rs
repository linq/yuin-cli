use crate::domain::{Collection, Platform, Track};
use serde_json::{Value, json};

pub(super) fn string(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| match &value[*key] {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}
pub(super) fn number(value: &Value, keys: &[&str]) -> f64 {
    keys.iter()
        .find_map(|key| {
            value[*key]
                .as_f64()
                .or_else(|| value[*key].as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or_default()
}
pub(super) fn array(value: &Value, keys: &[&str]) -> Vec<Value> {
    if keys.is_empty() {
        return value.as_array().cloned().unwrap_or_default();
    }
    keys.iter()
        .find_map(|key| value[*key].as_array().cloned())
        .unwrap_or_default()
}
fn object<'a>(value: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter()
        .map(|key| &value[*key])
        .find(|v| v.is_object())
        .unwrap_or(&Value::Null)
}
fn artists(value: &Value, keys: &[&str]) -> String {
    array(value, keys)
        .iter()
        .map(|a| string(a, &["name", "singerName"]))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("、")
}

pub fn clean(value: &str) -> String {
    use std::sync::LazyLock;
    static TAG: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new("<[^>]+>").expect("constant regex"));
    static ENTITY: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"&#(x[0-9a-fA-F]+|[0-9]+);").expect("constant regex"));
    let text = TAG.replace_all(value, "");
    let text = ENTITY.replace_all(&text, |caps: &regex::Captures<'_>| {
        let digits = &caps[1];
        let n = if let Some(hex) = digits.strip_prefix('x') {
            u32::from_str_radix(hex, 16).ok()
        } else {
            digits.parse::<u32>().ok()
        };
        n.and_then(char::from_u32)
            .map_or_else(|| caps[0].to_owned(), |c| c.to_string())
    });
    text.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .chars()
        .filter(|c| !c.is_control())
        .take(4096)
        .collect()
}

pub(super) fn track(r: &Value, p: Platform) -> Option<Track> {
    let id;
    let title;
    let mut artist;
    let album;
    let duration;
    let mut art = String::new();
    let mut info = json!({});
    let mut qualities = serde_json::Map::new();
    match p {
        Platform::Wy => {
            id = string(r, &["id"]);
            title = string(r, &["name"]);
            let a = object(r, &["album", "al"]);
            album = string(a, &["name"]);
            artist = artists(r, &["artists", "ar"]);
            duration = number(r, &["duration", "dt"]) / 1000.0;
            art = string(a, &["picUrl"]);
            info["albumId"] = json!(string(a, &["id"]));
            info["songmid"] = id.parse::<u64>().map_or_else(|_| json!(id), |n| json!(n));
            for (key, q) in [
                ("lMusic", "128k"),
                ("l", "128k"),
                ("hMusic", "320k"),
                ("h", "320k"),
                ("sq", "flac"),
                ("hr", "flac24bit"),
            ] {
                if r[key].is_object() {
                    qualities.insert(q.into(), json!({}));
                }
            }
        }
        Platform::Kw => {
            id = string(r, &["MUSICRID", "id"])
                .trim_start_matches("MUSIC_")
                .to_string();
            title = string(r, &["SONGNAME", "name"]);
            artist = string(r, &["ARTIST", "artist"]);
            album = string(r, &["ALBUM", "album"]);
            duration = number(r, &["DURATION", "duration"]);
            art = string(r, &["pic", "albumpic"]);
            info["albumId"] = json!(string(r, &["ALBUMID", "albumid"]));
            let minfo = string(r, &["N_MINFO", "n_minfo", "MINFO"]);
            for (bitrate, q) in [
                ("128", "128k"),
                ("320", "320k"),
                ("2000", "flac"),
                ("4000", "flac24bit"),
            ] {
                if minfo.contains(&format!("bitrate:{bitrate},")) {
                    qualities.insert(q.into(), json!({}));
                }
            }
        }
        Platform::Kg => {
            id = string(r, &["Audioid", "audio_id", "hash", "FileHash"]);
            let file = string(r, &["filename", "FileName"]);
            let parts = file.split(" - ").collect::<Vec<_>>();
            let name = string(r, &["OriSongName", "SongName"]);
            title = if name.is_empty() {
                parts.last().unwrap_or(&"").to_string()
            } else {
                name
            };
            artist = string(r, &["SingerName", "singername"]);
            if artist.is_empty() {
                artist = if parts.len() > 1 {
                    parts[0].into()
                } else {
                    artists(r, &["Singers"])
                };
            }
            album = string(r, &["AlbumName", "album_name"]);
            duration = number(r, &["Duration", "duration"]);
            info["albumId"] = json!(string(r, &["AlbumID", "album_id"]));
            info["hash"] = json!(string(r, &["FileHash", "hash"]));
            info["albumAudioId"] = json!(string(r, &["MixSongID", "album_audio_id"]));
            for (key, q) in [
                ("FileHash", "128k"),
                ("hash", "128k"),
                ("HQFileHash", "320k"),
                ("320hash", "320k"),
                ("SQFileHash", "flac"),
                ("sqhash", "flac"),
                ("ResFileHash", "flac24bit"),
            ] {
                let hash = string(r, &[key]);
                if !hash.is_empty() {
                    qualities.insert(q.into(), json!({"hash":hash}));
                }
            }
        }
        Platform::Tx => {
            id = string(r, &["mid", "songmid"]);
            title = string(r, &["title", "name", "songname"]);
            artist = artists(r, &["singer"]);
            let a = &r["album"];
            album = string(a, &["name"]);
            duration = number(r, &["interval"]);
            let mid = string(a, &["mid"]);
            if !mid.is_empty() {
                art = format!("https://y.gtimg.cn/music/photo_new/T002R500x500M000{mid}.jpg");
            }
            info["songId"] = json!(number(r, &["id", "songid"]) as u64);
            info["albumId"] = json!(mid);
            info["albumMid"] = json!(mid);
            info["strMediaMid"] = json!(string(&r["file"], &["media_mid"]));
            for (key, q) in [
                ("size_128mp3", "128k"),
                ("size_320mp3", "320k"),
                ("size_flac", "flac"),
                ("size_hires", "flac24bit"),
            ] {
                if number(&r["file"], &[key]) > 0.0 {
                    qualities.insert(q.into(), json!({}));
                }
            }
        }
        Platform::Mg => {
            id = string(r, &["songId", "id", "contentId"]);
            title = string(r, &["name", "songName", "songTitle"]);
            artist = artists(r, &["singers", "singerList", "artists"]);
            if artist.is_empty() {
                artist = string(r, &["singer", "singerName"]);
            }
            album = string(r, &["albumName", "album"]);
            art = string(r, &["img3", "mediumPic", "largePic", "img1"]);
            if !art.is_empty() && !art.starts_with("http") {
                art = format!("https://d.musicapp.migu.cn{art}");
            }
            if art.is_empty() {
                art = array(r, &["imgItems", "albumImgs"])
                    .first()
                    .map(|r| string(r, &["img"]))
                    .unwrap_or_default();
            }
            let d = number(r, &["duration"]);
            duration = if d > 0.0 {
                d
            } else {
                let value = string(r, &["length"]);
                let p = value.split(':').collect::<Vec<_>>();
                if p.len() == 2 {
                    p[0].parse::<f64>().unwrap_or(0.0) * 60.0 + p[1].parse::<f64>().unwrap_or(0.0)
                } else {
                    0.0
                }
            };
            for (key, aliases) in [
                ("copyrightId", vec!["copyrightId"]),
                ("albumId", vec!["albumId"]),
                ("lrcUrl", vec!["lrcUrl", "lyricUrl"]),
                ("mrcUrl", vec!["mrcurl"]),
                ("trcUrl", vec!["trcUrl"]),
            ] {
                info[key] = json!(string(r, &aliases));
            }
            for f in array(r, &["audioFormats", "rateFormats", "newRateFormats"]) {
                let key = string(&f, &["formatType"]);
                if let Some(q) = match key.as_str() {
                    "PQ" => Some("128k"),
                    "HQ" => Some("320k"),
                    "SQ" => Some("flac"),
                    "ZQ" | "ZQ24" => Some("flac24bit"),
                    _ => None,
                } {
                    qualities.insert(q.into(), json!({}));
                }
            }
        }
    }
    if id.is_empty() || title.is_empty() {
        return None;
    }
    info["types"] = json!(
        crate::config::QUALITIES
            .iter()
            .filter_map(|q| qualities.get(*q).map(|v| {
                let mut value = v.clone();
                value["type"] = json!(q);
                value
            }))
            .collect::<Vec<_>>()
    );
    info["_types"] = json!(qualities);
    Some(Track {
        id: format!("{p}:{id}"),
        song_id: id,
        platform: Some(p),
        title: clean(&title),
        artist: clean(&artist),
        album: clean(&album),
        duration,
        artwork: if art.is_empty() { None } else { Some(art) },
        path: String::new(),
        source_info: info,
    })
}

pub(super) fn collection(r: &Value, p: Platform, chart: bool) -> Option<Collection> {
    let (id, title, subtitle, art) = if chart {
        match p {
            Platform::Wy => (
                string(r, &["id"]),
                string(r, &["name"]),
                string(r, &["updateFrequency"]),
                string(r, &["coverImgUrl"]),
            ),
            Platform::Kw => (
                string(r, &["id"]),
                string(r, &["name"]),
                "酷我官方榜单".into(),
                String::new(),
            ),
            Platform::Kg => (
                string(r, &["rankid"]),
                string(r, &["rankname"]),
                String::new(),
                string(r, &["imgurl"]),
            ),
            Platform::Tx => (
                string(r, &["topId"]),
                string(r, &["title"]),
                String::new(),
                string(r, &["frontPicUrl"]),
            ),
            Platform::Mg => (
                string(r, &["rankId"]),
                string(r, &["rankName"]),
                String::new(),
                string(r, &["imageUrl"]),
            ),
        }
    } else {
        match p {
            Platform::Wy => (
                string(r, &["id"]),
                string(r, &["name"]),
                string(&r["creator"], &["nickname"]),
                string(r, &["coverImgUrl"]),
            ),
            Platform::Kw => (
                string(r, &["id"]),
                string(r, &["name"]),
                string(r, &["uname"]),
                string(r, &["img"]),
            ),
            Platform::Kg => (
                string(r, &["specialid"]),
                string(r, &["specialname"]),
                string(r, &["nickname"]),
                string(r, &["img", "imgurl"]),
            ),
            Platform::Tx => (
                string(r, &["tid"]),
                string(r, &["title"]),
                string(&r["creator"], &["nick"]),
                string(&r["cover"], &["medium_url"]),
            ),
            Platform::Mg => (
                string(r, &["resId"]),
                string(r, &["txt"]),
                string(r, &["txt2"]),
                string(r, &["img"]),
            ),
        }
    };
    if id.is_empty() || title.is_empty() {
        return None;
    }
    Some(Collection {
        id: format!("{p}:{id}"),
        title: clean(&title),
        subtitle: clean(&subtitle),
        artwork: if art.is_empty() {
            None
        } else {
            Some(art.replace("{size}", "400"))
        },
        platform: p,
        chart,
    })
}

pub(super) fn searched_collection(r: &Value, p: Platform) -> Option<Collection> {
    let (id, title, subtitle, artwork) = match p {
        Platform::Wy => (
            string(r, &["id"]),
            string(r, &["name"]),
            string(&r["creator"], &["nickname"]),
            string(r, &["coverImgUrl"]),
        ),
        Platform::Kw => (
            string(r, &["playlistid"]),
            string(r, &["name"]),
            string(r, &["nickname"]),
            string(r, &["pic"]),
        ),
        Platform::Kg => (
            string(r, &["specialid"]),
            string(r, &["specialname"]),
            string(r, &["nickname"]),
            string(r, &["imgurl"]).replace("{size}", "400"),
        ),
        Platform::Tx => (
            string(r, &["dissid"]),
            string(r, &["dissname"]),
            string(&r["creator"], &["name"]),
            string(r, &["imgurl"]),
        ),
        Platform::Mg => (
            string(r, &["id"]),
            string(r, &["name"]),
            string(r, &["userName"]),
            string(r, &["musicListPicUrl"]),
        ),
    };
    if id.is_empty() || title.is_empty() {
        return None;
    }
    Some(Collection {
        id: format!("{p}:{id}"),
        title: clean(&title),
        subtitle: clean(&subtitle),
        artwork: (!artwork.is_empty()).then_some(artwork),
        platform: p,
        chart: false,
    })
}
