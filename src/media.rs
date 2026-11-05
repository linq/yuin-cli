use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaReport {
    pub extension: String,
    pub codec: String,
    pub duration: f64,
    pub sample_rate: u32,
    pub bit_depth: Option<u32>,
    pub bitrate: u64,
    pub sha256: String,
    pub bytes: u64,
}

pub fn validate(path: &Path, quality: &str, cancel: &CancellationToken) -> Result<MediaReport> {
    use symphonia::core::{
        codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_ALAC, CODEC_ID_FLAC, CODEC_ID_MP3},
        formats::{TrackType, probe::Hint},
        io::MediaSourceStream,
    };
    let mut file = File::open(path)?;
    let bytes = file.metadata()?.len();
    ensure!(
        bytes > 16 && bytes <= 512 * 1024 * 1024,
        "Invalid audio file size"
    );
    let mut head = [0_u8; 64];
    let n = file.read(&mut head)?;
    let extension = if head.starts_with(b"fLaC") {
        "flac"
    } else if head.starts_with(b"RIFF") && head[8..12] == *b"WAVE" {
        "wav"
    } else if head.starts_with(b"ID3") || (head[0] == 0xff && head[1] & 0xe0 == 0xe0) {
        "mp3"
    } else if head.starts_with(b"OggS") {
        "ogg"
    } else if head[4..8] == *b"ftyp" {
        "m4a"
    } else {
        anyhow::bail!("Response is not a supported audio container (HTML/error pages are rejected)")
    };
    let flac_depth = if extension == "flac" && n >= 26 && head[4] & 0x7f == 0 {
        let packed = u64::from_be_bytes(head[18..26].try_into()?);
        Some(((packed >> 36) & 31) as u32 + 1)
    } else {
        None
    };
    file.seek(SeekFrom::Start(0))?;
    let mut format = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            MediaSourceStream::new(Box::new(file), Default::default()),
            Default::default(),
            Default::default(),
        )
        .context("Audio container cannot be parsed")?;
    let track = format
        .default_track(TrackType::Audio)
        .context("File has no audio track")?;
    let id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .context("Missing audio codec parameters")?;
    let codec = if params.codec == CODEC_ID_FLAC {
        "flac"
    } else if params.codec == CODEC_ID_MP3 {
        "mp3"
    } else if params.codec == CODEC_ID_ALAC {
        "alac"
    } else if params.codec == CODEC_ID_AAC {
        "aac"
    } else if extension == "wav" {
        "pcm"
    } else {
        "other"
    };
    let bit_depth = if codec == "flac" {
        flac_depth
    } else {
        params.bits_per_sample
    };
    if quality == "flac" || quality == "flac24bit" {
        ensure!(
            codec == "flac",
            "Quality mismatch: requested {quality}, received {codec}"
        );
    }
    if quality == "flac24bit" {
        ensure!(
            flac_depth.is_some_and(|bits| bits >= 24),
            "Quality mismatch: FLAC STREAMINFO does not confirm 24-bit audio"
        );
    }
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &Default::default())
        .context("Unsupported audio codec")?;
    let mut duration = 0.0;
    let mut sample_rate = 0;
    let begin = Instant::now();
    while let Some(packet) = format
        .next_packet()
        .context("Truncated or invalid audio container")?
    {
        ensure!(!cancel.is_cancelled(), "Download cancelled");
        ensure!(
            begin.elapsed() < Duration::from_secs(120),
            "Audio validation exceeded 120 seconds"
        );
        if packet.track_id != id {
            continue;
        }
        let decoded = decoder
            .decode(&packet)
            .context("Audio cannot be decoded completely")?;
        sample_rate = decoded.spec().rate();
        ensure!(sample_rate > 0, "Invalid sample rate");
        duration += decoded.frames() as f64 / sample_rate as f64;
        ensure!(duration <= 86400.0, "Audio exceeds 24 hours");
    }
    ensure!(duration > 0.0, "File contains no decodable audio samples");
    let bitrate = (bytes as f64 * 8.0 / duration) as u64;
    if codec != "flac" && codec != "alac" {
        let requested = match quality {
            "128k" => 128_000,
            "320k" => 320_000,
            _ => 0,
        };
        ensure!(
            bitrate >= requested * 95 / 100,
            "Quality mismatch: requested {quality}, measured average {bitrate} bit/s"
        );
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut block = [0_u8; 65536];
    loop {
        ensure!(!cancel.is_cancelled(), "Download cancelled");
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
    }
    Ok(MediaReport {
        extension: extension.into(),
        codec: codec.into(),
        duration,
        sample_rate,
        bit_depth,
        bitrate,
        sha256: hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    #[test]
    fn flac_quality_is_checked_against_encoded_streaminfo_not_decoded_float_samples() {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/flac.json")).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        for bits in [16, 24] {
            let file = directory.path().join(format!("{bits}.flac"));
            std::fs::write(
                &file,
                STANDARD
                    .decode(fixtures[bits.to_string()].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
            let report = validate(&file, "flac", &cancel).unwrap();
            assert_eq!(report.bit_depth, Some(bits));
            assert_eq!(report.codec, "flac");
            assert_eq!(validate(&file, "flac24bit", &cancel).is_ok(), bits == 24);
        }
    }
}
