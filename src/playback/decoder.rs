use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use symphonia::core::{
    codecs::audio::AudioDecoder,
    formats::{FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::{MediaSource, MediaSourceStream},
    units::{Time, TimeBase},
};
pub struct Decoder {
    format: Option<Box<dyn FormatReader>>,
    codec: Box<dyn AudioDecoder>,
    track: u32,
    time_base: Option<TimeBase>,
    pub duration: Option<f64>,
    pub rate: u32,
    pub channels: u16,
    skip_until: f64,
    position: u64,
    end: bool,
}
impl Decoder {
    pub fn new(input: Box<dyn MediaSource>, progressive: bool) -> Result<Self> {
        // The generic probe otherwise seeks to trailing tags before identifying the
        // container, forcing progressive HTTP streams to download all their bytes.
        // Seeking itself remains available (needed by MP4 container initialization).
        let probing = Arc::new(AtomicBool::new(progressive));
        let input = ProbeSource {
            input,
            probing: probing.clone(),
        };
        let format = symphonia::default::get_probe()
            .probe(
                &Hint::new(),
                MediaSourceStream::new(Box::new(input), Default::default()),
                Default::default(),
                Default::default(),
            )
            .context("Unsupported or invalid audio container")?;
        probing.store(false, Ordering::Release);
        let track = format
            .default_track(TrackType::Audio)
            .context("File has no audio track")?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .context("Missing audio codec parameters")?;
        let codec = symphonia::default::get_codecs()
            .make_audio_decoder(params, &Default::default())
            .context("Unsupported audio codec")?;
        let duration = track
            .time_base
            .zip(track.duration)
            .and_then(|(base, duration)| base.calc_duration(duration))
            .map(|time| time.as_secs_f64());
        Ok(Self {
            track: track.id,
            time_base: track.time_base,
            duration,
            rate: 0,
            channels: 0,
            format: Some(format),
            codec,
            skip_until: 0.0,
            position: 0,
            end: false,
        })
    }
    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        if self.duration.is_some_and(|duration| seconds >= duration) {
            self.end = true;
            return Ok(());
        }
        // MP4 readers can exhaust their atom iterator at EOF. Re-probe before
        // seeking back into a completed container instead of reusing that iterator.
        if self.end {
            let mut source = self
                .format
                .take()
                .context("Audio container is unavailable")?
                .into_inner();
            source.seek(SeekFrom::Start(0))?;
            self.format = Some(symphonia::default::get_probe().probe(
                &Hint::new(),
                source,
                Default::default(),
                Default::default(),
            )?);
        }
        self.format
            .as_mut()
            .context("Audio container is unavailable")?
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::try_from_secs_f64(seconds).context("Invalid seek time")?,
                    track_id: Some(self.track),
                },
            )
            .context("Audio stream cannot seek to the requested position")?;
        self.codec.reset();
        self.skip_until = seconds;
        self.position = (seconds * self.rate as f64) as u64;
        self.end = false;
        Ok(())
    }
    pub fn next(&mut self) -> Result<Option<(u64, Vec<f32>)>> {
        while !self.end {
            let Some(packet) = self
                .format
                .as_mut()
                .context("Audio container is unavailable")?
                .next_packet()
                .context("Invalid or truncated audio stream")?
            else {
                self.end = true;
                return Ok(None);
            };
            if packet.track_id != self.track {
                continue;
            }
            let decoded = self
                .codec
                .decode(&packet)
                .context("Audio decoding failed")?;
            let rate = decoded.spec().rate();
            let channels = decoded.spec().channels().count() as u16;
            ensure!(
                rate > 0 && channels > 0 && channels <= 32,
                "Invalid audio stream specification"
            );
            ensure!(
                self.rate == 0 || (self.rate == rate && self.channels == channels),
                "Audio format changed during playback"
            );
            self.rate = rate;
            self.channels = channels;
            let start = self
                .time_base
                .and_then(|base| base.calc_time(packet.pts))
                .map(|time| time.as_secs_f64())
                .unwrap_or(self.position as f64 / rate as f64);
            let skip = ((self.skip_until - start).max(0.0) * rate as f64).round() as usize;
            if skip >= decoded.frames() {
                continue;
            }
            let mut samples = Vec::new();
            decoded.slice(skip..).copy_to_vec_interleaved(&mut samples);
            let position = (start * rate as f64).round() as u64 + skip as u64;
            self.position = position + samples.len() as u64 / channels as u64;
            return Ok(Some((position, samples)));
        }
        Ok(None)
    }
}

struct ProbeSource {
    input: Box<dyn MediaSource>,
    probing: Arc<AtomicBool>,
}
impl Read for ProbeSource {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.input.read(bytes)
    }
}
impl Seek for ProbeSource {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        self.input.seek(from)
    }
}
impl MediaSource for ProbeSource {
    fn is_seekable(&self) -> bool {
        self.input.is_seekable()
    }
    fn byte_len(&self) -> Option<u64> {
        if self.probing.load(Ordering::Acquire) {
            None
        } else {
            self.input.byte_len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::io::Cursor;
    #[test]
    fn mp3_aac_and_16_24_bit_flac_decode_and_seek() {
        let formats: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/audio-formats.json")).unwrap();
        let flac: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/flac.json")).unwrap();
        for encoded in [&formats["mp3"], &formats["m4a"], &flac["16"], &flac["24"]] {
            let bytes = STANDARD.decode(encoded.as_str().unwrap()).unwrap();
            let mut decoder = Decoder::new(Box::new(Cursor::new(bytes)), false).unwrap();
            let mut frames = 0;
            while let Some((_, samples)) = decoder.next().unwrap() {
                assert!(samples.iter().all(|sample| sample.is_finite()));
                frames += samples.len() / decoder.channels as usize;
            }
            assert!(frames > 0);
            assert!(decoder.rate > 0);
            decoder.seek(0.0).unwrap();
            let beginning = decoder.next().unwrap().unwrap().0;
            assert!(beginning as f64 / (decoder.rate as f64) < 0.003);
            let duration = frames as f64 / decoder.rate as f64;
            decoder.seek(duration / 2.0).unwrap();
            let halfway = decoder.next().unwrap().unwrap().0 as f64 / decoder.rate as f64;
            assert!(
                (halfway - duration / 2.0).abs() < 0.003,
                "{halfway} / {duration}"
            );
        }
    }
}
