//! Media state exchanged with the native browser player. No simulated clock.
//! Media Source Extensions keep their coded frames here: scripts append and
//! remove them, and the player reads them to decode.
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use nebula_media::fmp4::{Codec, Demuxer, Event};

pub struct MediaState {
    pub src: String,
    pub paused: bool,
    pub ended: bool,
    pub current_time: f64,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub ready_state: u8,
    pub error: Option<String>,
    pub seek: Option<f64>,
    pub seeking: bool,
    pub muted: Option<bool>,
    pub volume: f64,
    pub revision: u64,
    /// The MediaSource attached to the element, if any.
    pub source: Option<u32>,
    pub playback_rate: f64,
    /// Whether the player is waiting for data it doesn't have.
    pub stalled: bool,
}

impl Default for MediaState {
    fn default() -> Self {
        Self {
            src: String::new(),
            paused: true,
            ended: false,
            current_time: 0.0,
            duration: f64::NAN,
            width: 0,
            height: 0,
            ready_state: 0,
            error: None,
            seek: None,
            seeking: false,
            muted: None,
            volume: 1.0,
            revision: 0,
            source: None,
            playback_rate: 1.0,
            stalled: false,
        }
    }
}

/// What `blob:` URLs stand for.
pub enum ObjectUrl {
    Blob { bytes: Arc<[u8]>, kind: String },
    MediaSource(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackKind {
    Video,
    Audio,
    Other,
}

/// A coded frame (times in microseconds, presentation order by `pts`).
#[derive(Clone)]
pub struct CodedFrame {
    pub pts: i64,
    pub dts: i64,
    pub duration: i64,
    pub sync: bool,
    pub data: Arc<[u8]>,
    /// The decoder configuration in force when it was appended.
    pub config: Arc<[u8]>,
}

pub struct MseTrack {
    pub id: u32,
    pub kind: TrackKind,
    /// avcC or AudioSpecificConfig.
    pub config: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// Coded frames by decode time.
    pub frames: BTreeMap<i64, CodedFrame>,
    /// Bumped whenever frames are removed or replaced (a player that read
    /// past them starts over from a keyframe).
    pub revision: u64,
    /// After a discontinuity, frames are dropped until a keyframe.
    need_sync: bool,
    last_dts: Option<i64>,
}

impl MseTrack {
    /// Buffered presentation ranges (µs), merging gaps up to a frame long.
    pub fn ranges(&self) -> Vec<(i64, i64)> {
        let mut spans: Vec<(i64, i64)> = self.frames.values().map(|f| (f.pts, f.pts + f.duration.max(1))).collect();
        spans.sort_unstable();
        let mut out: Vec<(i64, i64)> = Vec::new();
        let tolerance = self.frames.values().next().map_or(0, |f| f.duration.max(1000)) * 2;
        for (s, e) in spans {
            match out.last_mut() {
                Some(last) if s <= last.1 + tolerance => last.1 = last.1.max(e),
                _ => out.push((s, e)),
            }
        }
        out
    }
}

pub struct SourceBuffer {
    pub id: u32,
    pub mime: String,
    demux: Demuxer,
    pub tracks: Vec<MseTrack>,
    pub timestamp_offset: f64,
    pub window_start: f64,
    pub window_end: f64,
    pub sequence: bool,
    /// Whether an initialization segment has been appended.
    pub initialized: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadyState {
    Closed,
    Open,
    Ended,
}

pub struct MediaSource {
    pub id: u32,
    pub ready: ReadyState,
    pub duration: f64,
    pub buffers: Vec<SourceBuffer>,
    /// The media element it's attached to.
    pub element: Option<crate::dom::NodeId>,
    /// Bumped by every append and removal (players poll it).
    pub revision: u64,
}

/// Total coded bytes one MediaSource may hold (QuotaExceededError beyond).
pub const MSE_QUOTA: usize = 160 << 20;

/// Whether `mime` is something this browser can play through MSE or a
/// media element: H.264 Constrained Baseline/Main/High (8-bit 4:2:0) and
/// AAC-LC, in MP4. `width`/`height` parameters above 1920×1080 are refused.
pub fn supported_type(mime: &str) -> bool {
    let lower = mime.to_ascii_lowercase();
    let mut parts = lower.split(';').map(str::trim);
    let container = parts.next().unwrap_or("");
    if !matches!(container, "video/mp4" | "audio/mp4") {
        return false;
    }
    let mut codecs_ok = true;
    for p in parts {
        let Some((k, v)) = p.split_once('=') else { continue };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
        match k.trim() {
            "codecs" => {
                codecs_ok = v.split(',').map(str::trim).all(|c| {
                    if let Some(rest) = c.strip_prefix("avc1.").or_else(|| c.strip_prefix("avc3.")) {
                        let profile = u8::from_str_radix(rest.get(0..2).unwrap_or(""), 16).unwrap_or(0);
                        container == "video/mp4" && matches!(profile, 0x42 | 0x4d | 0x58 | 0x64)
                    } else {
                        matches!(c, "mp4a.40.2" | "mp4a.40.02" | "mp4a.40" | "mp4a.67")
                    }
                });
            }
            "width" => codecs_ok &= v.parse::<u32>().is_ok_and(|w| w <= 1920),
            "height" => codecs_ok &= v.parse::<u32>().is_ok_and(|h| h <= 1088),
            "framerate" => codecs_ok &= v.parse::<f64>().is_ok_and(|f| f <= 60.0),
            "bitrate" => codecs_ok &= v.parse::<u64>().is_ok_and(|b| b <= 20_000_000),
            "channels" => codecs_ok &= v.parse::<u32>().is_ok_and(|c| c <= 2),
            // Spatial audio, HDR and other feature probes.
            "eotf" => codecs_ok &= v == "bt709",
            "spatialrendering" => codecs_ok &= v == "false",
            "decode-to-texture" | "experimental" | "features" => codecs_ok = false,
            _ => {}
        }
    }
    codecs_ok
}

const US: f64 = 1_000_000.0;

/// One track of a file the browser demuxed itself (see [`MediaSource::from_tracks`]).
pub struct DemuxedTrack {
    pub kind: TrackKind,
    /// The decoder configuration (avcC or AudioSpecificConfig).
    pub config: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub frames: Vec<CodedFrame>,
}

impl MediaSource {
    pub fn new(id: u32) -> MediaSource {
        MediaSource {
            id,
            ready: ReadyState::Closed,
            duration: f64::NAN,
            buffers: Vec::new(),
            element: None,
            revision: 0,
        }
    }

    pub fn buffer(&mut self, id: u32) -> Option<&mut SourceBuffer> {
        self.buffers.iter_mut().find(|b| b.id == id)
    }

    pub fn add_buffer(&mut self, id: u32, mime: &str) -> Result<(), &'static str> {
        if !supported_type(mime) {
            return Err("NotSupportedError");
        }
        if self.ready != ReadyState::Open {
            return Err("InvalidStateError");
        }
        if self.buffers.len() >= 4 {
            return Err("QuotaExceededError");
        }
        self.buffers.push(SourceBuffer {
            id,
            mime: String::from(mime),
            demux: Demuxer::new(),
            tracks: Vec::new(),
            timestamp_offset: 0.0,
            window_start: 0.0,
            window_end: f64::INFINITY,
            sequence: false,
            initialized: false,
        });
        Ok(())
    }

    pub fn bytes(&self) -> usize {
        self.buffers.iter().flat_map(|b| &b.tracks).flat_map(|t| t.frames.values()).map(|f| f.data.len()).sum()
    }

    /// Appends bytes to a SourceBuffer: Err is the appendBuffer error
    /// (a decode error ends the stream).
    pub fn append(&mut self, id: u32, bytes: &[u8]) -> Result<(), &'static str> {
        if self.bytes() + bytes.len() > MSE_QUOTA {
            return Err("QuotaExceededError");
        }
        if self.ready == ReadyState::Ended {
            self.ready = ReadyState::Open;
        }
        let duration = self.duration;
        let buf = self.buffer(id).ok_or("InvalidStateError")?;
        let events = buf.demux.push(bytes).map_err(|_| "decode")?;
        let mut new_duration = None;
        for e in events {
            match e {
                Event::Init(init) => {
                    if !buf.initialized && duration.is_nan() {
                        new_duration = Some(init.duration.unwrap_or(f64::INFINITY));
                    }
                    for t in &init.tracks {
                        let (kind, config, width, height) = match &t.codec {
                            Codec::Avc { config, width, height } => {
                                (TrackKind::Video, config.clone(), *width as u32, *height as u32)
                            }
                            Codec::Aac { config, .. } => (TrackKind::Audio, config.clone(), 0, 0),
                            Codec::Other(_) => (TrackKind::Other, Vec::new(), 0, 0),
                        };
                        match buf.tracks.iter_mut().find(|x| x.id == t.id) {
                            Some(existing) => {
                                existing.config = Arc::from(config);
                                existing.width = width;
                                existing.height = height;
                                existing.need_sync = true;
                            }
                            None => buf.tracks.push(MseTrack {
                                id: t.id,
                                kind,
                                config: Arc::from(config),
                                width,
                                height,
                                frames: BTreeMap::new(),
                                revision: 0,
                                need_sync: true,
                                last_dts: None,
                            }),
                        }
                    }
                    buf.initialized = true;
                }
                Event::Frames(frames) => {
                    let offset = (buf.timestamp_offset * US) as i64;
                    let (ws, we) = ((buf.window_start * US) as i64, buf.window_end * US);
                    // Coded frames replace what was buffered over their span.
                    let mut spans: BTreeMap<u32, (i64, i64)> = BTreeMap::new();
                    let mut accepted = Vec::new();
                    for f in frames {
                        let Some(t) = buf.tracks.iter_mut().find(|t| t.id == f.track) else { continue };
                        let (pts, dts) = (f.pts + offset, f.dts + offset);
                        if pts < ws || (pts + f.duration) as f64 > we {
                            t.need_sync = true;
                            continue;
                        }
                        if t.last_dts.is_some_and(|last| dts <= last || dts > last + 2 * US as i64) {
                            t.need_sync = true;
                        }
                        if t.need_sync && !f.sync {
                            continue;
                        }
                        t.need_sync = false;
                        t.last_dts = Some(dts);
                        let span = spans.entry(f.track).or_insert((pts, pts + f.duration));
                        span.0 = span.0.min(pts);
                        span.1 = span.1.max(pts + f.duration);
                        let config = t.config.clone();
                        accepted.push((
                            f.track,
                            CodedFrame { pts, dts, duration: f.duration, sync: f.sync, data: f.data, config },
                        ));
                    }
                    for (track, (start, end)) in spans {
                        if let Some(t) = buf.tracks.iter_mut().find(|t| t.id == track) {
                            remove_span(t, start, end, false);
                        }
                    }
                    for (track, f) in accepted {
                        if let Some(t) = buf.tracks.iter_mut().find(|t| t.id == track) {
                            t.frames.insert(f.dts, f);
                        }
                    }
                }
            }
        }
        if let Some(d) = new_duration {
            self.duration = d;
        }
        self.revision += 1;
        Ok(())
    }

    /// SourceBuffer.remove(start, end), in seconds.
    pub fn remove(&mut self, id: u32, start: f64, end: f64) {
        if let Some(buf) = self.buffer(id) {
            let (s, e) = ((start * US) as i64, if end.is_finite() { (end * US) as i64 } else { i64::MAX });
            for t in &mut buf.tracks {
                remove_span(t, s, e, true);
            }
        }
        self.revision += 1;
    }

    pub fn abort(&mut self, id: u32) {
        if let Some(buf) = self.buffer(id) {
            buf.demux.reset();
            for t in &mut buf.tracks {
                t.need_sync = true;
                t.last_dts = None;
            }
        }
    }

    /// A SourceBuffer's buffered ranges in seconds: the intersection over its tracks.
    pub fn buffered(&self, id: u32) -> Vec<(f64, f64)> {
        let Some(buf) = self.buffers.iter().find(|b| b.id == id) else { return Vec::new() };
        intersect(buf.tracks.iter().filter(|t| t.kind != TrackKind::Other).map(|t| t.ranges()))
    }

    /// The element's buffered ranges: the intersection over all buffers.
    pub fn element_buffered(&self) -> Vec<(f64, f64)> {
        intersect(
            self.buffers.iter().flat_map(|b| &b.tracks).filter(|t| t.kind != TrackKind::Other).map(|t| t.ranges()),
        )
    }

    /// Whether every buffer has its initialization segment (HAVE_METADATA).
    pub fn has_metadata(&self) -> bool {
        !self.buffers.is_empty() && self.buffers.iter().all(|b| b.initialized)
    }

    /// A source for a whole file the browser demuxed itself: one buffer
    /// per track, already ended.
    pub fn from_tracks(id: u32, duration: f64, tracks: Vec<DemuxedTrack>) -> MediaSource {
        let mut ms = MediaSource::new(id);
        ms.duration = duration;
        ms.ready = ReadyState::Ended;
        for (i, DemuxedTrack { kind, config, width, height, frames }) in tracks.into_iter().enumerate() {
            let config: Arc<[u8]> = Arc::from(config);
            let frames = frames
                .into_iter()
                .map(|mut f| {
                    f.config = config.clone();
                    (f.dts, f)
                })
                .collect();
            ms.buffers.push(SourceBuffer {
                id: i as u32 + 1,
                mime: String::new(),
                demux: Demuxer::new(),
                tracks: alloc::vec![MseTrack {
                    id: i as u32 + 1,
                    kind,
                    config,
                    width,
                    height,
                    frames,
                    revision: 0,
                    need_sync: false,
                    last_dts: None
                }],
                timestamp_offset: 0.0,
                window_start: 0.0,
                window_end: f64::INFINITY,
                sequence: false,
                initialized: true,
            });
        }
        ms
    }

    pub fn track(&self, kind: TrackKind) -> Option<&MseTrack> {
        self.buffers.iter().flat_map(|b| &b.tracks).find(|t| t.kind == kind)
    }
}

/// Removes frames presented in [start, end); with `dependents`, also the
/// frames after them up to the next keyframe (they can't be decoded).
fn remove_span(t: &mut MseTrack, start: i64, end: i64, dependents: bool) {
    let doomed: Vec<i64> =
        t.frames.values().filter(|f| f.pts < end && f.pts + f.duration > start).map(|f| f.dts).collect();
    if doomed.is_empty() {
        return;
    }
    let first = *doomed.iter().min().unwrap();
    for d in doomed {
        t.frames.remove(&d);
    }
    // Frames decoded after the first removed one depend on it, up to the
    // next keyframe (B-frames interleave, so go by decode order).
    if dependents || t.kind == TrackKind::Video {
        let after: Vec<i64> = t.frames.range(first..).take_while(|(_, f)| !f.sync).map(|(d, _)| *d).collect();
        for d in after {
            t.frames.remove(&d);
        }
    }
    t.revision += 1;
}

fn intersect(mut sets: impl Iterator<Item = Vec<(i64, i64)>>) -> Vec<(f64, f64)> {
    let Some(mut acc) = sets.next() else { return Vec::new() };
    for set in sets {
        let mut out = Vec::new();
        for &(a0, a1) in &acc {
            for &(b0, b1) in &set {
                let (s, e) = (a0.max(b0), a1.min(b1));
                if s < e {
                    out.push((s, e));
                }
            }
        }
        acc = out;
    }
    acc.into_iter().map(|(s, e)| (s as f64 / US, e as f64 / US)).collect()
}
