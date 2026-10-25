//! Decoder threads: open a source (file, HTTP, ffmpeg fallback), decode,
//! convert to the output format and feed the track's ring.

use std::fs::File;
use std::io;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track as SymTrack};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, StandardTagKey, Tag};
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

use super::ffmpeg::{FfInput, Ffmpeg};
use super::http::{self, LiveSource, Opened};
use super::icy::IcyReader;
use super::output::OutFormat;
use super::replaygain::ReplayGainTags;
use super::resample::{remix, Resampler};
use super::track::{Track, EOF, FAILED, OPENED, PRIMED, STOP};
use super::{EngineEvent, Source};
use crate::view::StreamFormat;

pub type Emit = Arc<dyn Fn(EngineEvent) + Send + Sync>;

/// Live-stream reconnect attempts after a mid-stream network error.
const MAX_RECONNECTS: u32 = 3;

/// PCM producer at the source's native format.
pub trait Pcm: Send {
    /// Append the next decoded chunk (interleaved f32). `Ok(false)` at end of stream.
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> anyhow::Result<bool>;
    /// Native (sample rate, channels) of the data returned by the last chunk.
    fn spec(&self) -> (u32, usize);
    fn seek(&mut self, pos: Duration) -> anyhow::Result<()>;
    /// A new in-band title (chained Ogg streams).
    fn take_title(&mut self) -> Option<String> {
        None
    }
}

pub fn frames_at(d: Duration, rate: u32) -> u64 {
    (d.as_secs_f64() * f64::from(rate)).round() as u64
}

/// Spawn the decoder thread for `track`.
pub fn spawn(track: Arc<Track>, out: OutFormat, emit: Emit) {
    let (t, e) = (track.clone(), emit.clone());
    let spawned = thread::Builder::new()
        .name("rustune-decode".into())
        .spawn(move || run(&t, out, &e));
    if let Err(err) = spawned {
        fail(&track, &emit, anyhow!("cannot start decoder thread: {err}"));
    }
}

fn fail(track: &Track, emit: &Emit, err: anyhow::Error) {
    if !track.has(STOP) {
        emit(EngineEvent::Error {
            token: track.token,
            message: format!("{err:#}"),
        });
    }
    track.set(FAILED | EOF | PRIMED);
}

/// Channel remap + sample-rate conversion from a source spec to the output.
struct Converter {
    input: (u32, usize),
    out: OutFormat,
    mixed: Vec<f32>,
    resampler: Option<Resampler>,
}

impl Converter {
    fn new(input: (u32, usize), out: OutFormat) -> Self {
        let (rate, ch) = input;
        let resampler = (rate > 0 && rate != out.rate)
            .then(|| Resampler::new(rate, out.rate, ch.max(1).min(out.channels)));
        Self {
            input,
            out,
            mixed: Vec::new(),
            resampler,
        }
    }

    fn convert(&mut self, data: &[f32], dst: &mut Vec<f32>) {
        let in_ch = self.input.1.max(1);
        let out_ch = self.out.channels;
        match self.resampler.as_mut() {
            None => remix(data, in_ch, out_ch, dst),
            Some(r) if in_ch == out_ch => r.process(data, dst),
            // Resample at the smaller channel count.
            Some(r) if in_ch < out_ch => {
                self.mixed.clear();
                r.process(data, &mut self.mixed);
                remix(&self.mixed, in_ch, out_ch, dst);
            }
            Some(r) => {
                self.mixed.clear();
                remix(data, in_ch, out_ch, &mut self.mixed);
                r.process(&self.mixed, dst);
            }
        }
    }

    fn finish(&mut self, dst: &mut Vec<f32>) {
        let in_ch = self.input.1.max(1);
        let out_ch = self.out.channels;
        match self.resampler.as_mut() {
            Some(r) if in_ch < out_ch => {
                self.mixed.clear();
                r.finish(&mut self.mixed);
                remix(&self.mixed, in_ch, out_ch, dst);
            }
            Some(r) => r.finish(dst),
            None => {}
        }
    }

    fn reset(&mut self) {
        if let Some(r) = self.resampler.as_mut() {
            r.reset();
        }
    }
}

fn push(track: &Track, data: &[f32], seen_seek: u64) {
    if !data.is_empty() {
        track.ring.push_all(data, &|| {
            track.has(STOP) || track.seek_generation() != seen_seek
        });
    }
    if !track.has(PRIMED) && track.ring.len() >= track.ring.threshold() {
        track.set(PRIMED);
    }
}

fn run(track: &Arc<Track>, out: OutFormat, emit: &Emit) {
    track.ring.register_producer();
    let mut src = match open(track, out, emit) {
        Ok(s) => s,
        Err(e) => return fail(track, emit, e),
    };
    track.set(OPENED);
    let live = matches!(track.src, Source::Url(_)) && !track.info().seekable;
    let mut conv = Converter::new(src.spec(), out);
    let mut native = Vec::with_capacity(1 << 14);
    let mut converted = Vec::with_capacity(1 << 14);
    let mut seen_seek = 0u64;
    let mut produced = 0u64;
    let mut first = true;
    let mut reconnects = 0;

    if track.start_ms > 0 && src.seek(Duration::from_millis(track.start_ms)).is_ok() {
        produced = frames_at(Duration::from_millis(track.start_ms), out.rate);
        track
            .played
            .store(produced, std::sync::atomic::Ordering::Relaxed);
    }

    loop {
        let flags = track.flags();
        if flags & STOP != 0 {
            return;
        }
        let (generation, ms) = track.seek_request();
        if generation != seen_seek {
            seen_seek = generation;
            let target = Duration::from_millis(ms);
            let past_end = track.info().duration.is_some_and(|d| target >= d);
            track.clear(EOF);
            let ok = !past_end && src.seek(target).is_ok();
            conv.reset();
            if ok || past_end {
                produced = frames_at(target, out.rate);
            }
            track.ring.flush(produced, generation);
            if past_end {
                track.set(PRIMED | EOF);
            }
            continue;
        }
        if flags & EOF != 0 {
            thread::park();
            continue;
        }
        native.clear();
        converted.clear();
        match src.next_chunk(&mut native) {
            Ok(true) => {
                reconnects = 0;
                if let Some(title) = src.take_title() {
                    if !track.has(STOP) {
                        emit(EngineEvent::StreamTitle {
                            token: track.token,
                            title,
                        });
                    }
                }
                let spec = src.spec();
                if spec != conv.input {
                    conv = Converter::new(spec, out);
                }
                if first {
                    first = false;
                    track.update_info(|i| {
                        i.format.sample_rate.get_or_insert(spec.0);
                        i.format.channels.get_or_insert(spec.1 as u16);
                    });
                }
                conv.convert(&native, &mut converted);
                produced += (converted.len() / out.channels) as u64;
                push(track, &converted, seen_seek);
            }
            Ok(false) => {
                conv.finish(&mut converted);
                produced += (converted.len() / out.channels) as u64;
                push(track, &converted, seen_seek);
                track.set(PRIMED | EOF);
            }
            Err(e) if live && produced > 0 && reconnects < MAX_RECONNECTS => {
                reconnects += 1;
                thread::park_timeout(Duration::from_secs(1));
                if track.has(STOP) {
                    return;
                }
                match open(track, out, emit) {
                    Ok(s) => {
                        src = s;
                        conv = Converter::new(src.spec(), out);
                    }
                    Err(_) => return fail(track, emit, e),
                }
            }
            Err(e) => return fail(track, emit, e),
        }
    }
}

fn open(track: &Arc<Track>, out: OutFormat, emit: &Emit) -> anyhow::Result<Box<dyn Pcm>> {
    match &track.src {
        Source::File(path) => {
            let file =
                File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
            let byte_len = file.metadata().ok().map(|m| m.len());
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            match Native::open(Box::new(file), ext.as_deref(), None) {
                Ok((n, info)) => {
                    publish(track, &info, byte_len, None);
                    Ok(Box::new(n))
                }
                Err(e) => ffmpeg(track, out, emit, FfInput::Path(path.clone()), e.into()),
            }
        }
        Source::Url(url) => open_url(track, out, emit, url),
    }
}

fn open_url(
    track: &Arc<Track>,
    out: OutFormat,
    emit: &Emit,
    url: &str,
) -> anyhow::Result<Box<dyn Pcm>> {
    let src = match http::open(url) {
        Ok(s) => s,
        // HTTP status errors are final; protocol oddities (e.g. Shoutcast's
        // "ICY 200 OK" status line) get a second chance through ffmpeg.
        Err(e) if is_http_status(&e) => return Err(e),
        Err(e) => return ffmpeg(track, out, emit, FfInput::Url(url.to_owned()), e),
    };
    let ext = http::url_extension(&src.url);
    match src.opened {
        Opened::Hls => ffmpeg(track, out, emit, FfInput::Url(src.url), anyhow!("HLS")),
        Opened::Seekable(reader) => {
            let len = reader.byte_len();
            match Native::open(Box::new(reader), ext.as_deref(), src.mime.as_deref()) {
                Ok((n, info)) => {
                    publish(track, &info, len, src.kbps);
                    Ok(Box::new(n))
                }
                Err(e) => ffmpeg(track, out, emit, FfInput::Url(src.url), e.into()),
            }
        }
        Opened::Stream { body, icy_metaint } => {
            let media: Box<dyn MediaSource> = match icy_metaint {
                Some(n) => Box::new(LiveSource::new(IcyReader::new(
                    body,
                    n,
                    title_sink(track, emit),
                ))),
                None => Box::new(LiveSource::new(body)),
            };
            match Native::open(media, ext.as_deref(), src.mime.as_deref()) {
                Ok((n, info)) => {
                    publish(track, &info, None, src.kbps);
                    Ok(Box::new(n))
                }
                Err(e) if icy_metaint.is_some() => {
                    // Reconnect and pipe through ffmpeg so ICY titles keep flowing.
                    let again = http::open(&src.url)?;
                    let input = match again.opened {
                        Opened::Stream {
                            body,
                            icy_metaint: Some(n),
                        } => FfInput::Pipe(Box::new(IcyReader::new(
                            body,
                            n,
                            title_sink(track, emit),
                        ))),
                        _ => FfInput::Url(again.url),
                    };
                    let kbps = src.kbps;
                    let pcm = ffmpeg(track, out, emit, input, e.into())?;
                    if kbps.is_some() {
                        track.update_info(|i| i.format.kbps = i.format.kbps.or(kbps));
                    }
                    Ok(pcm)
                }
                Err(e) => ffmpeg(track, out, emit, FfInput::Url(src.url), e.into()),
            }
        }
    }
}

fn is_http_status(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        matches!(
            c.downcast_ref::<ureq::Error>(),
            Some(ureq::Error::StatusCode(_))
        )
    })
}

/// Fall back to ffmpeg; if it is unavailable, report the native error.
fn ffmpeg(
    track: &Arc<Track>,
    out: OutFormat,
    emit: &Emit,
    input: FfInput,
    native_err: anyhow::Error,
) -> anyhow::Result<Box<dyn Pcm>> {
    match Ffmpeg::start(track.clone(), emit.clone(), input, out) {
        Ok(f) => Ok(Box::new(f)),
        Err(e)
            if e.downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            Err(native_err.context("unsupported format (install ffmpeg to play it)"))
        }
        Err(e) => Err(e),
    }
}

fn title_sink(track: &Arc<Track>, emit: &Emit) -> Box<dyn FnMut(&str) + Send> {
    let (track, emit) = (track.clone(), emit.clone());
    Box::new(move |title: &str| {
        if !track.has(STOP) {
            emit(EngineEvent::StreamTitle {
                token: track.token,
                title: title.to_owned(),
            });
        }
    })
}

/// Facts learned while opening a native source.
pub struct NativeInfo {
    pub duration: Option<Duration>,
    pub rate: Option<u32>,
    pub channels: Option<u16>,
    pub bits: Option<u32>,
    pub seekable: bool,
    pub rg: ReplayGainTags,
}

fn publish(track: &Track, info: &NativeInfo, byte_len: Option<u64>, kbps_hint: Option<u32>) {
    let kbps = kbps_hint
        .or_else(|| match (byte_len, info.duration) {
            (Some(b), Some(d)) if d.as_secs_f64() > 0.5 => {
                Some((b as f64 * 8.0 / d.as_secs_f64() / 1000.0).round() as u32)
            }
            _ => None,
        })
        .or_else(|| {
            let (r, c, b) = (info.rate?, info.channels?, info.bits?);
            Some(r * u32::from(c) * b / 1000)
        });
    track.set_replaygain(&info.rg);
    track.update_info(|i| {
        i.duration = info.duration;
        i.format = StreamFormat {
            kbps,
            sample_rate: info.rate,
            channels: info.channels,
        };
        i.seekable = info.seekable;
        i.replaygain_db = info.rg.preferred_db();
    });
}

/// Symphonia-based decoder.
pub struct Native {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    buf: Option<SampleBuffer<f32>>,
    spec: (u32, usize),
    /// After an accurate seek: drop decoded frames before this timestamp.
    skip_to: u64,
    bad: u32,
    title: Option<String>,
}

fn pick_track(tracks: &[SymTrack]) -> Option<&SymTrack> {
    tracks
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some())
        .or_else(|| {
            tracks
                .iter()
                .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        })
}

fn title_from_tags(tags: &[Tag]) -> Option<String> {
    let get = |key: StandardTagKey| {
        tags.iter()
            .find(|t| t.std_key == Some(key))
            .map(|t| t.value.to_string())
            .filter(|s| !s.trim().is_empty())
    };
    match (get(StandardTagKey::Artist), get(StandardTagKey::TrackTitle)) {
        (Some(a), Some(t)) => Some(format!("{a} - {t}")),
        (None, Some(t)) => Some(t),
        _ => None,
    }
}

impl Native {
    pub fn open(
        src: Box<dyn MediaSource>,
        ext: Option<&str>,
        mime: Option<&str>,
    ) -> Result<(Self, NativeInfo), SymError> {
        let seekable = src.is_seekable();
        let mss = MediaSourceStream::new(src, MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        if let Some(e) = ext {
            hint.with_extension(e);
        }
        if let Some(m) = mime {
            hint.mime_type(m);
        }
        let fmt_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let mut probed = symphonia::default::get_probe().format(
            &hint,
            mss,
            &fmt_opts,
            &MetadataOptions::default(),
        )?;
        let mut rg = ReplayGainTags::default();
        if let Some(meta) = probed.metadata.get() {
            if let Some(rev) = meta.current() {
                rg.merge_tags(rev.tags());
            }
        }
        let mut format = probed.format;
        if let Some(rev) = format.metadata().current() {
            rg.merge_tags(rev.tags());
        }
        let track = pick_track(format.tracks()).ok_or(SymError::Unsupported("no audio track"))?;
        let track_id = track.id;
        let params = track.codec_params.clone();
        let decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default())?;
        let duration = params
            .n_frames
            .and_then(|n| match (params.time_base, params.sample_rate) {
                (Some(tb), _) => Some(Duration::from(tb.calc_time(n))),
                (None, Some(r)) if r > 0 => Some(Duration::from_secs_f64(n as f64 / f64::from(r))),
                _ => None,
            });
        let info = NativeInfo {
            duration,
            rate: params.sample_rate,
            channels: params.channels.map(|c| c.count() as u16),
            bits: params.bits_per_sample,
            seekable,
            rg,
        };
        let native = Self {
            format,
            decoder,
            track_id,
            time_base: params.time_base,
            buf: None,
            spec: (
                params.sample_rate.unwrap_or(0),
                params.channels.map_or(0, |c| c.count()),
            ),
            skip_to: 0,
            bad: 0,
            title: None,
        };
        Ok((native, info))
    }

    /// Chained streams (e.g. Ogg radio) switch tracks: rebuild the decoder.
    fn reset_track(&mut self) -> Result<(), SymError> {
        let track =
            pick_track(self.format.tracks()).ok_or(SymError::Unsupported("no audio track"))?;
        self.track_id = track.id;
        self.time_base = track.codec_params.time_base;
        let params = track.codec_params.clone();
        self.decoder =
            symphonia::default::get_codecs().make(&params, &DecoderOptions::default())?;
        if let Some(rev) = self.format.metadata().skip_to_latest() {
            if let Some(t) = title_from_tags(rev.tags()) {
                self.title = Some(t);
            }
        }
        Ok(())
    }
}

fn ts_to_frames(time_base: Option<TimeBase>, ts: u64, rate: u32) -> u64 {
    match time_base {
        Some(tb) => {
            let t = tb.calc_time(ts);
            ((t.seconds as f64 + t.frac) * f64::from(rate)).round() as u64
        }
        None => ts,
    }
}

impl Pcm for Native {
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> anyhow::Result<bool> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Ok(false)
                }
                Err(SymError::ResetRequired) => {
                    self.reset_track()?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let ts = packet.ts();
            let skip_ts = self.skip_to.saturating_sub(ts);
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                    self.bad += 1;
                    if self.bad > 100 {
                        bail!("too many corrupt frames");
                    }
                    continue;
                }
                Err(SymError::ResetRequired) => {
                    self.reset_track()?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let spec = *decoded.spec();
            let frames = decoded.frames();
            if frames == 0 {
                continue;
            }
            let ch = spec.channels.count();
            let needs_new = self.buf.as_ref().is_none_or(|b| b.capacity() < frames * ch);
            if needs_new {
                self.buf = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
            }
            let Some(buf) = self.buf.as_mut() else {
                continue;
            };
            buf.copy_interleaved_ref(decoded);
            self.bad = 0;
            let skip = if skip_ts > 0 {
                (ts_to_frames(self.time_base, skip_ts, spec.rate) as usize).min(frames)
            } else {
                0
            };
            if skip >= frames {
                continue;
            }
            self.spec = (spec.rate, ch);
            out.extend_from_slice(&buf.samples()[skip * ch..frames * ch]);
            return Ok(true);
        }
    }

    fn spec(&self) -> (u32, usize) {
        self.spec
    }

    fn seek(&mut self, pos: Duration) -> anyhow::Result<()> {
        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from(pos),
                track_id: Some(self.track_id),
            },
        )?;
        self.decoder.reset();
        self.skip_to = seeked.required_ts;
        Ok(())
    }

    fn take_title(&mut self) -> Option<String> {
        self.title.take()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 16-bit PCM WAV bytes with a sine on every channel.
    pub fn wav_bytes(rate: u32, channels: u16, frames: usize, freq: f32) -> Vec<u8> {
        let data_len = frames * usize::from(channels) * 2;
        let mut v = Vec::with_capacity(44 + data_len);
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        v.extend_from_slice(&(channels * 2).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data_len as u32).to_le_bytes());
        for i in 0..frames {
            let s = (0.5 * (std::f32::consts::TAU * freq * i as f32 / rate as f32).sin() * 32767.0)
                as i16;
            for _ in 0..channels {
                v.extend_from_slice(&s.to_le_bytes());
            }
        }
        v
    }

    fn decode_all(n: &mut Native) -> Vec<f32> {
        let mut out = Vec::new();
        while n.next_chunk(&mut out).unwrap() {}
        out
    }

    #[test]
    fn native_decodes_wav_with_duration_and_seek() {
        let bytes = wav_bytes(8_000, 2, 8_000, 440.0);
        let src = Box::new(io::Cursor::new(bytes));
        let (mut n, info) = Native::open(src, Some("wav"), None).unwrap();
        assert_eq!(info.duration, Some(Duration::from_secs(1)));
        assert_eq!(info.rate, Some(8_000));
        assert_eq!(info.channels, Some(2));
        assert!(info.seekable);
        let all = decode_all(&mut n);
        assert_eq!(n.spec(), (8_000, 2));
        assert_eq!(all.len(), 16_000);

        // Accurate seek to 0.5 s yields exactly the second half.
        n.seek(Duration::from_millis(500)).unwrap();
        let half = decode_all(&mut n);
        assert_eq!(half.len(), 8_000);
        assert_eq!(&half[..], &all[8_000..]);
    }

    #[test]
    fn native_rejects_garbage() {
        let src = Box::new(io::Cursor::new(vec![0x55u8; 4096]));
        assert!(Native::open(src, None, None).is_err());
    }

    #[test]
    fn converter_resamples_and_remixes() {
        let out = OutFormat {
            rate: 48_000,
            channels: 2,
        };
        let mut c = Converter::new((24_000, 1), out);
        let input = vec![0.25f32; 24_000];
        let mut dst = Vec::new();
        c.convert(&input, &mut dst);
        c.finish(&mut dst);
        assert_eq!(dst.len(), 2 * 48_000);
        // Steady state is unity gain on both channels.
        assert!(dst[2_000..90_000].iter().all(|s| (s - 0.25).abs() < 1e-3));
    }

    /// Decodes `bytes` once, returning the native chunks and their spec.
    fn decode_chunks(bytes: &[u8], ext: &str) -> (Vec<Vec<f32>>, (u32, usize)) {
        let src = Box::new(io::Cursor::new(bytes.to_vec()));
        let (mut n, _) = Native::open(src, Some(ext), None).unwrap();
        let mut chunks = Vec::new();
        loop {
            let mut c = Vec::new();
            if !n.next_chunk(&mut c).unwrap() {
                break;
            }
            chunks.push(c);
        }
        (chunks, n.spec())
    }

    /// Best of five runs of `f`, which returns the frames it produced.
    fn best_of(mut f: impl FnMut() -> usize) -> (std::time::Duration, usize) {
        let mut best = std::time::Duration::MAX;
        let mut frames = 0;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            frames = std::hint::black_box(f());
            best = best.min(t.elapsed());
        }
        (best, frames)
    }

    fn bench_one(name: &str, bytes: &[u8], ext: &str) {
        let (chunks, spec) = decode_chunks(bytes, ext);
        let native_frames: usize = chunks.iter().map(|c| c.len() / spec.1).sum();
        let secs = native_frames as f64 / f64::from(spec.0);
        println!(
            "bench {name}: {secs:.1} s at {} Hz, {} ch, {} chunks",
            spec.0,
            spec.1,
            chunks.len()
        );
        let report = |what: &str, (t, frames): (std::time::Duration, usize)| {
            println!(
                "  {what:<34} {:>8.1} ms {:>8.1} ns/out-frame {:>7.3} % of a core",
                t.as_secs_f64() * 1e3,
                t.as_secs_f64() * 1e9 / frames.max(1) as f64,
                100.0 * t.as_secs_f64() / secs
            );
        };
        let decode = |convert: Option<OutFormat>| {
            let copy = bytes.to_vec();
            move || {
                let src = Box::new(io::Cursor::new(copy.clone()));
                let (mut n, _) = Native::open(src, Some(ext), None).unwrap();
                let mut native = Vec::with_capacity(1 << 14);
                let mut converted = Vec::with_capacity(1 << 14);
                let mut conv = convert.map(|o| Converter::new(n.spec(), o));
                let mut frames = 0;
                loop {
                    native.clear();
                    converted.clear();
                    let more = n.next_chunk(&mut native).unwrap();
                    let stale = more && conv.as_ref().is_some_and(|c| c.input != n.spec());
                    if let (true, Some(o)) = (stale, convert) {
                        conv = Some(Converter::new(n.spec(), o));
                    }
                    match conv.as_mut() {
                        Some(c) if more => c.convert(&native, &mut converted),
                        Some(c) => c.finish(&mut converted),
                        None => converted.extend_from_slice(&native),
                    }
                    frames += converted.len() / 2;
                    let _ = std::hint::black_box(&converted);
                    if !more {
                        return frames;
                    }
                }
            }
        };
        let chunks = &chunks;
        let convert_only = |o: OutFormat| {
            move || {
                let mut conv = Converter::new(spec, o);
                let mut converted = Vec::with_capacity(1 << 14);
                let mut frames = 0;
                for c in chunks {
                    converted.clear();
                    conv.convert(c, &mut converted);
                    frames += converted.len() / o.channels;
                    let _ = std::hint::black_box(&converted);
                }
                frames
            }
        };
        let out48 = OutFormat {
            rate: 48_000,
            channels: 2,
        };
        let native_out = OutFormat {
            rate: spec.0,
            channels: 2,
        };
        report("decode only", best_of(decode(None)));
        report(
            "decode + convert to 48 kHz stereo",
            best_of(decode(Some(out48))),
        );
        report(
            "decode + convert, rates match",
            best_of(decode(Some(native_out))),
        );
        report(
            "convert only to 48 kHz stereo",
            best_of(convert_only(out48)),
        );
        report(
            "convert only, rates match",
            best_of(convert_only(native_out)),
        );
    }

    /// Decoder-path throughput: decode only, decode + remix/resample to a
    /// 48 kHz device, and the conversion alone. Set `RUSTUNE_BENCH_MP3` to an
    /// MP3 to include it; a generated WAV is always measured.
    #[test]
    #[ignore = "benchmark: cargo test --release -- --ignored --nocapture bench_"]
    fn bench_decode_path() {
        if let Ok(path) = std::env::var("RUSTUNE_BENCH_MP3") {
            bench_one("mp3", &std::fs::read(&path).unwrap(), "mp3");
        }
        bench_one("wav", &wav_bytes(44_100, 2, 44_100 * 60, 440.0), "wav");
    }
}
