//! Incremental fragmented MP4 (ISO BMFF byte stream) demuxing for Media
//! Source Extensions: initialization segments (`moov`) give the tracks,
//! media segments (`moof` + `mdat`) give timed samples. Bytes may arrive in
//! any split; only whole top-level boxes are processed.
use crate::Result;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// Bytes waiting for the rest of their box are bounded (a fragment larger
/// than this is refused rather than buffered forever).
const MAX_BOX: usize = 64 << 20;
const MAX_SAMPLES_PER_RUN: usize = 100_000;

#[derive(Clone, Debug, PartialEq)]
pub enum Codec {
    /// H.264: the `avcC` configuration record and coded size.
    Avc { config: Vec<u8>, width: u16, height: u16 },
    /// AAC: the AudioSpecificConfig.
    Aac { config: Vec<u8>, channels: u16, sample_rate: u32 },
    /// A track this player can't decode (its four-character code).
    Other([u8; 4]),
}

#[derive(Clone, Debug)]
pub struct Track {
    pub id: u32,
    pub timescale: u32,
    pub codec: Codec,
    default_duration: u32,
    default_size: u32,
    default_flags: u32,
}

#[derive(Clone, Debug)]
pub struct Init {
    pub tracks: Vec<Track>,
    /// From `mehd` or `mvhd`, in seconds (None: unknown).
    pub duration: Option<f64>,
}

/// A coded frame; times in microseconds.
#[derive(Clone, Debug)]
pub struct Frame {
    pub track: u32,
    pub dts: i64,
    pub pts: i64,
    pub duration: i64,
    pub sync: bool,
    pub data: Arc<[u8]>,
}

pub enum Event {
    Init(Init),
    Frames(Vec<Frame>),
}

#[derive(Default)]
pub struct Demuxer {
    buf: Vec<u8>,
    init: Option<Init>,
    /// The last moof's runs: (track index, base offset in the moof, frames
    /// without data yet: (data offset from moof start, size)).
    moof: Option<Moof>,
}

struct Moof {
    /// Bytes from the start of the moof to the start of the following mdat's payload.
    size: usize,
    runs: Vec<(Frame, usize, usize)>,
}

impl Demuxer {
    pub fn new() -> Demuxer {
        Demuxer::default()
    }

    pub fn init(&self) -> Option<&Init> {
        self.init.as_ref()
    }

    /// Discards a partial box (SourceBuffer.abort / resetParserState).
    pub fn reset(&mut self) {
        self.buf.clear();
        self.moof = None;
    }

    /// Whether bytes of an unfinished box are waiting.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Event>> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut at = 0;
        while self.buf.len() - at >= 8 {
            let data = &self.buf[at..];
            let short = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = data[4..8].try_into().unwrap();
            let (size, header) = match short {
                1 => {
                    if data.len() < 16 {
                        break;
                    }
                    let n = u64::from_be_bytes(data[8..16].try_into().unwrap());
                    (usize::try_from(n).map_err(|_| "MP4 box too large")?, 16)
                }
                0 => return Err("MP4 boxes of unknown size are unsupported in a stream"),
                n => (n, 8),
            };
            if size < header || size > MAX_BOX {
                self.buf.clear();
                return Err("invalid MP4 box size");
            }
            if data.len() < size {
                break;
            }
            let body = &data[header..size];
            match &kind {
                b"moov" => {
                    let init = parse_init(body)?;
                    events.push(Event::Init(init.clone()));
                    self.init = Some(init);
                }
                b"moof" => {
                    let init = self.init.as_ref().ok_or("media segment before initialization segment")?;
                    self.moof = Some(parse_moof(body, size, init)?);
                }
                b"mdat" => {
                    if let Some(moof) = self.moof.take() {
                        let mut frames = Vec::with_capacity(moof.runs.len());
                        for (mut f, offset, len) in moof.runs {
                            // Offsets count from the start of the moof; the mdat payload
                            // starts `moof.size + header` bytes in.
                            let start = offset.checked_sub(moof.size + header).ok_or("sample data outside mdat")?;
                            let bytes = body.get(start..start + len).ok_or("sample data outside mdat")?;
                            f.data = Arc::from(bytes);
                            frames.push(f);
                        }
                        events.push(Event::Frames(frames));
                    }
                }
                // ftyp, styp, sidx, emsg, prft, free, skip…: nothing to play.
                _ => {}
            }
            // A moof's offsets reach past its own box only into the mdat
            // that follows; anything between them is counted in its size.
            if &kind != b"moof" {
                if let Some(m) = &mut self.moof {
                    m.size += size;
                }
            }
            at += size;
        }
        self.buf.drain(..at);
        Ok(events)
    }
}

#[derive(Clone, Copy)]
struct Atom<'a> {
    kind: [u8; 4],
    data: &'a [u8],
}

fn atoms(mut data: &[u8]) -> Result<Vec<Atom<'_>>> {
    let mut out = Vec::new();
    while data.len() >= 8 {
        let short = u32_at(data, 0)? as usize;
        let (size, header) = match short {
            0 => (data.len(), 8),
            1 => (usize::try_from(u64_at(data, 8)?).map_err(|_| "MP4 box too large")?, 16),
            n => (n, 8),
        };
        if size < header || size > data.len() {
            return Err("truncated MP4 box");
        }
        out.push(Atom { kind: data[4..8].try_into().unwrap(), data: &data[header..size] });
        data = &data[size..];
    }
    Ok(out)
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<&'a [u8]> {
    optional(data, kind)?.ok_or("missing MP4 box")
}

fn optional<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>> {
    Ok(atoms(data)?.into_iter().find(|a| &a.kind == kind).map(|a| a.data))
}

fn u16_at(data: &[u8], p: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(data.get(p..p + 2).ok_or("truncated MP4 value")?.try_into().unwrap()))
}

fn u32_at(data: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(data.get(p..p + 4).ok_or("truncated MP4 value")?.try_into().unwrap()))
}

fn u64_at(data: &[u8], p: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(data.get(p..p + 8).ok_or("truncated MP4 value")?.try_into().unwrap()))
}

fn parse_init(moov: &[u8]) -> Result<Init> {
    let mut duration = None;
    if let Some(mvhd) = optional(moov, b"mvhd")? {
        let (scale, d) = if mvhd.first() == Some(&1) {
            (u32_at(mvhd, 20)?, u64_at(mvhd, 24)?)
        } else {
            (u32_at(mvhd, 12)?, u32_at(mvhd, 16)? as u64)
        };
        if scale > 0 && d > 0 && d != u32::MAX as u64 && d != u64::MAX {
            duration = Some(d as f64 / scale as f64);
        }
        if let Some(mehd) = optional(moov, b"mvex")?.map(|mvex| optional(mvex, b"mehd")).transpose()?.flatten() {
            let d = if mehd.first() == Some(&1) { u64_at(mehd, 4)? } else { u32_at(mehd, 4)? as u64 };
            if scale > 0 && d > 0 {
                duration = Some(d as f64 / scale as f64);
            }
        }
    }
    let mut trex = Vec::new();
    if let Some(mvex) = optional(moov, b"mvex")? {
        for a in atoms(mvex)?.into_iter().filter(|a| a.kind == *b"trex") {
            trex.push((u32_at(a.data, 4)?, u32_at(a.data, 12)?, u32_at(a.data, 16)?, u32_at(a.data, 20)?));
        }
    }
    let mut tracks = Vec::new();
    for trak in atoms(moov)?.into_iter().filter(|a| a.kind == *b"trak") {
        let tkhd = child(trak.data, b"tkhd")?;
        let id = u32_at(tkhd, if tkhd.first() == Some(&1) { 20 } else { 12 })?;
        let mdia = child(trak.data, b"mdia")?;
        let mdhd = child(mdia, b"mdhd")?;
        let timescale = u32_at(mdhd, if mdhd.first() == Some(&1) { 20 } else { 12 })?;
        if timescale == 0 {
            return Err("zero MP4 timebase");
        }
        let stsd = child(child(child(mdia, b"minf")?, b"stbl")?, b"stsd")?;
        let entry = atoms(stsd.get(8..).ok_or("truncated sample description")?)?
            .into_iter()
            .next()
            .ok_or("missing sample description")?;
        let codec = match &entry.kind {
            b"avc1" | b"avc3" => {
                let width = u16_at(entry.data, 24)?;
                let height = u16_at(entry.data, 26)?;
                let config = child(entry.data.get(78..).ok_or("truncated AVC description")?, b"avcC")?.to_vec();
                Codec::Avc { config, width, height }
            }
            b"mp4a" => {
                let channels = u16_at(entry.data, 16)?;
                let sample_rate = u32_at(entry.data, 24)? >> 16;
                let esds = child(entry.data.get(28..).ok_or("truncated mp4a entry")?, b"esds")?;
                let config = crate::mp4::decoder_config(esds.get(4..).ok_or("truncated esds")?, 0)?
                    .ok_or("missing AAC configuration")?
                    .to_vec();
                Codec::Aac { config, channels, sample_rate }
            }
            other => Codec::Other(*other),
        };
        let (_, default_duration, default_size, default_flags) =
            trex.iter().copied().find(|t| t.0 == id).unwrap_or((id, 0, 0, 0));
        tracks.push(Track { id, timescale, codec, default_duration, default_size, default_flags });
    }
    if tracks.is_empty() {
        return Err("initialization segment has no tracks");
    }
    Ok(Init { tracks, duration })
}

const SAMPLE_NON_SYNC: u32 = 0x0001_0000;

fn parse_moof(moof: &[u8], moof_size: usize, init: &Init) -> Result<Moof> {
    let mut runs = Vec::new();
    for traf in atoms(moof)?.into_iter().filter(|a| a.kind == *b"traf") {
        let tfhd = child(traf.data, b"tfhd")?;
        let flags = u32_at(tfhd, 0)? & 0xff_ffff;
        let id = u32_at(tfhd, 4)?;
        let Some(track) = init.tracks.iter().find(|t| t.id == id) else { continue };
        let mut at = 8;
        let base = 0usize;
        if flags & 1 != 0 {
            // An explicit base offset refers to a whole file; in a byte
            // stream, offsets count from the moof regardless.
            at += 8;
        }
        if flags & 2 != 0 {
            at += 4;
        }
        let mut default_duration = track.default_duration;
        let mut default_size = track.default_size;
        let mut default_flags = track.default_flags;
        if flags & 8 != 0 {
            default_duration = u32_at(tfhd, at)?;
            at += 4;
        }
        if flags & 0x10 != 0 {
            default_size = u32_at(tfhd, at)?;
            at += 4;
        }
        if flags & 0x20 != 0 {
            default_flags = u32_at(tfhd, at)?;
        }
        let mut decode_time: u64 = match optional(traf.data, b"tfdt")? {
            Some(t) if t.first() == Some(&1) => u64_at(t, 4)?,
            Some(t) => u32_at(t, 4)? as u64,
            None => 0,
        };
        let scale = track.timescale as i128;
        let us = |t: i128| (t * 1_000_000 / scale) as i64;
        let mut next_offset = base;
        for trun in atoms(traf.data)?.into_iter().filter(|a| a.kind == *b"trun") {
            let t = trun.data;
            let version = t[0];
            let tf = u32_at(t, 0)? & 0xff_ffff;
            let count = u32_at(t, 4)? as usize;
            if count > MAX_SAMPLES_PER_RUN {
                return Err("too many samples in a track run");
            }
            let mut p = 8;
            let mut offset = next_offset;
            if tf & 1 != 0 {
                let rel = u32_at(t, p)? as i32 as i64;
                offset = (base as i64 + rel).max(0) as usize;
                p += 4;
            }
            let mut first_flags = None;
            if tf & 4 != 0 {
                first_flags = Some(u32_at(t, p)?);
                p += 4;
            }
            for i in 0..count {
                let mut duration = default_duration;
                let mut size = default_size;
                let mut sflags = if i == 0 { first_flags.unwrap_or(default_flags) } else { default_flags };
                let mut cto: i64 = 0;
                if tf & 0x100 != 0 {
                    duration = u32_at(t, p)?;
                    p += 4;
                }
                if tf & 0x200 != 0 {
                    size = u32_at(t, p)?;
                    p += 4;
                }
                if tf & 0x400 != 0 {
                    sflags = u32_at(t, p)?;
                    p += 4;
                }
                if tf & 0x800 != 0 {
                    let raw = u32_at(t, p)?;
                    cto = if version == 0 { raw as i64 } else { raw as i32 as i64 };
                    p += 4;
                }
                let size = size as usize;
                if size > 16 << 20 {
                    return Err("MP4 sample too large");
                }
                let dts = decode_time as i128;
                let sync = sflags & SAMPLE_NON_SYNC == 0 && (sflags >> 24) & 3 != 1;
                let frame = Frame {
                    track: id,
                    dts: us(dts),
                    pts: us(dts + cto as i128),
                    duration: us(duration as i128),
                    sync: match track.codec {
                        Codec::Avc { .. } => sync,
                        _ => true,
                    },
                    data: Arc::from(&[][..]),
                };
                runs.push((frame, offset, size));
                offset += size;
                decode_time += duration as u64;
            }
            next_offset = offset;
        }
    }
    Ok(Moof { size: moof_size, runs })
}
