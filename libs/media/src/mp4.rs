//! Bounded ISO BMFF sample-table demuxing for ordinary AVC MP4 files.
use crate::Result;
use alloc::vec::Vec;

const MAX_SAMPLES: usize = 250_000;

#[derive(Clone, Debug)]
pub struct Sample {
    pub offset: usize,
    pub size: usize,
    pub time_ms: u64,
    pub duration_ms: u64,
    pub sync: bool,
    pub trim_ms: u64,
}

pub struct Video {
    pub width: u16,
    pub height: u16,
    pub duration_ms: u64,
    pub config: Vec<u8>,
    pub samples: Vec<Sample>,
    pub has_audio: bool,
    pub audio: Option<Audio>,
}

pub struct Audio {
    pub config: Vec<u8>,
    pub samples: Vec<Sample>,
}

#[derive(Clone, Copy)]
struct Atom<'a> {
    kind: [u8; 4],
    data: &'a [u8],
}

fn atoms(mut data: &[u8]) -> Result<Vec<Atom<'_>>> {
    let mut out = Vec::new();
    while !data.is_empty() {
        if data.len() < 8 || out.len() >= MAX_SAMPLES {
            return Err("invalid MP4 boxes");
        }
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
    atoms(data)?.into_iter().find(|a| &a.kind == kind).map(|a| a.data).ok_or("missing MP4 box")
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
fn count(data: &[u8], width: usize) -> Result<usize> {
    let n = u32_at(data, 4)? as usize;
    if n > MAX_SAMPLES || 8 + n * width > data.len() {
        return Err("invalid MP4 table length");
    }
    Ok(n)
}

impl Video {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let moov = child(data, b"moov")?;
        let tracks = atoms(moov)?;
        let mut video = None;
        let mut has_audio = false;
        let mut audio = None;
        for atom in tracks.iter().filter(|a| a.kind == *b"trak") {
            let mdia = child(atom.data, b"mdia")?;
            let handler = child(mdia, b"hdlr")?;
            match handler.get(8..12) {
                Some(b"soun") => {
                    has_audio = true;
                    if audio.is_none() {
                        audio = parse_audio(data, mdia)
                            .and_then(|mut a| {
                                apply_edit(atom.data, mdia, &mut a.samples)?;
                                Ok(a)
                            })
                            .ok();
                    }
                }
                Some(b"vide") if video.is_none() => {
                    let mut v = parse_track(data, mdia)?;
                    apply_edit(atom.data, mdia, &mut v.samples)?;
                    video = Some(v);
                }
                _ => {}
            }
        }
        let mut video = video.ok_or("MP4 contains no supported video track")?;
        video.has_audio = has_audio;
        video.audio = audio;
        Ok(video)
    }
}

fn apply_edit(trak: &[u8], mdia: &[u8], samples: &mut [Sample]) -> Result<()> {
    let Some(edts) = optional(trak, b"edts")? else { return Ok(()) };
    let elst = child(edts, b"elst")?;
    if u32_at(elst, 4)? != 1 {
        return Err("multiple MP4 edits are unsupported");
    }
    let (time, rate) = match elst.first() {
        Some(0) => (u32_at(elst, 12)? as i32 as i64, u32_at(elst, 16)?),
        Some(1) => (u64_at(elst, 16)? as i64, u32_at(elst, 24)?),
        _ => return Err("unsupported MP4 edit version"),
    };
    if time < 0 || rate != 0x10000 {
        return Err("unsupported MP4 edit rate or empty edit");
    }
    let mdhd = child(mdia, b"mdhd")?;
    let scale = u32_at(mdhd, if mdhd.first() == Some(&1) { 20 } else { 12 })? as u64;
    if scale == 0 {
        return Err("zero edit timebase");
    }
    let shift = (time as u64).checked_mul(1000).ok_or("edit timestamp overflow")? / scale;
    for s in samples {
        s.trim_ms = shift.saturating_sub(s.time_ms).min(s.duration_ms);
        s.duration_ms -= s.trim_ms;
        s.time_ms = s.time_ms.saturating_sub(shift);
    }
    Ok(())
}

fn parse_track(file: &[u8], mdia: &[u8]) -> Result<Video> {
    let (duration_ms, samples) = parse_samples(file, mdia)?;
    let stbl = child(child(mdia, b"minf")?, b"stbl")?;
    let stsd = child(stbl, b"stsd")?;
    if u32_at(stsd, 4)? != 1 {
        return Err("multiple MP4 sample descriptions are unsupported");
    }
    let descriptions = atoms(stsd.get(8..).ok_or("missing sample description")?)?;
    let desc = descriptions.first().ok_or("missing AVC sample description")?;
    if !matches!(&desc.kind, b"avc1" | b"avc3") {
        return Err("MP4 video codec is not H.264");
    }
    let width = u16_at(desc.data, 24)?;
    let height = u16_at(desc.data, 26)?;
    if width == 0 || height == 0 || width as usize > crate::MAX_WIDTH || height as usize > crate::MAX_HEIGHT {
        return Err("video dimensions exceed playback limits");
    }
    let config = child(desc.data.get(78..).ok_or("truncated AVC description")?, b"avcC")?.to_vec();
    Ok(Video { width, height, duration_ms, config, samples, has_audio: false, audio: None })
}

fn parse_audio(file: &[u8], mdia: &[u8]) -> Result<Audio> {
    let stbl = child(child(mdia, b"minf")?, b"stbl")?;
    let stsd = child(stbl, b"stsd")?;
    if u32_at(stsd, 4)? != 1 {
        return Err("unsupported audio sample descriptions");
    }
    let descs = atoms(stsd.get(8..).ok_or("truncated audio description")?)?;
    let desc = descs.first().ok_or("missing audio description")?;
    if desc.kind != *b"mp4a" || u16_at(desc.data, 8)? != 0 {
        return Err("unsupported audio codec");
    }
    let esds = child(desc.data.get(28..).ok_or("truncated mp4a entry")?, b"esds")?;
    let config =
        decoder_config(esds.get(4..).ok_or("truncated esds")?, 0)?.ok_or("missing AAC configuration")?.to_vec();
    crate::AudioDecoder::new(&config)?;
    Ok(Audio { config, samples: parse_samples(file, mdia)?.1 })
}

pub(crate) fn decoder_config(mut bytes: &[u8], depth: usize) -> Result<Option<&[u8]>> {
    if depth > 4 {
        return Err("nested MP4 descriptors");
    }
    while !bytes.is_empty() {
        let tag = bytes[0];
        bytes = &bytes[1..];
        let mut size = 0usize;
        for i in 0..4 {
            let b = *bytes.first().ok_or("truncated descriptor length")?;
            bytes = &bytes[1..];
            size = (size << 7) | (b & 127) as usize;
            if b & 128 == 0 {
                break;
            }
            if i == 3 {
                return Err("invalid descriptor length");
            }
        }
        let payload = bytes.get(..size).ok_or("truncated descriptor")?;
        bytes = &bytes[size..];
        if tag == 5 {
            return Ok(Some(payload));
        }
        let offset = match tag {
            3 => {
                let flags = *payload.get(2).ok_or("truncated ES descriptor")?;
                let mut at = 3;
                if flags & 128 != 0 {
                    at += 2;
                }
                if flags & 64 != 0 {
                    at += 1 + *payload.get(at).ok_or("truncated ES URL")? as usize;
                }
                if flags & 32 != 0 {
                    at += 2;
                }
                at
            }
            4 => 13,
            _ => continue,
        };
        if let Some(c) = decoder_config(payload.get(offset..).ok_or("truncated ES descriptor")?, depth + 1)? {
            return Ok(Some(c));
        }
    }
    Ok(None)
}

fn parse_samples(file: &[u8], mdia: &[u8]) -> Result<(u64, Vec<Sample>)> {
    let mdhd = child(mdia, b"mdhd")?;
    let version = *mdhd.first().ok_or("missing MP4 timebase")?;
    let (scale, duration) = match version {
        0 => (u32_at(mdhd, 12)? as u64, u32_at(mdhd, 16)? as u64),
        1 => (u32_at(mdhd, 20)? as u64, u64_at(mdhd, 24)?),
        _ => return Err("unsupported MP4 timebase"),
    };
    if scale == 0 {
        return Err("zero MP4 timebase");
    }
    let stbl = child(child(mdia, b"minf")?, b"stbl")?;
    let stsz = child(stbl, b"stsz")?;
    let fixed = u32_at(stsz, 4)? as usize;
    let n = u32_at(stsz, 8)? as usize;
    if n == 0 || n > MAX_SAMPLES {
        return Err("invalid MP4 sample count");
    }
    if fixed == 0 && stsz.len() < 12 + n * 4 {
        return Err("truncated sample sizes");
    }
    let mut sizes = Vec::with_capacity(n);
    for i in 0..n {
        let size = if fixed == 0 { u32_at(stsz, 12 + i * 4)? as usize } else { fixed };
        if size == 0 || size > 8 << 20 {
            return Err("invalid video sample size");
        }
        sizes.push(size);
    }
    let stts = child(stbl, b"stts")?;
    let mut times = Vec::with_capacity(n);
    let mut decode_time = 0u64;
    for i in 0..count(stts, 8)? {
        let repeat = u32_at(stts, 8 + i * 8)? as usize;
        let delta = u32_at(stts, 12 + i * 8)? as u64;
        if repeat > n - times.len() || delta == 0 {
            return Err("invalid decode time table");
        }
        for _ in 0..repeat {
            times.push((decode_time, delta));
            decode_time = decode_time.checked_add(delta).ok_or("MP4 duration overflow")?;
        }
    }
    if times.len() != n {
        return Err("incomplete decode time table");
    }
    let mut offsets = alloc::vec![0i64; n];
    if let Some(ctts) = optional(stbl, b"ctts")? {
        if *ctts.first().ok_or("truncated composition time table")? > 1 {
            return Err("unsupported composition time table");
        }
        let mut index = 0;
        for i in 0..count(ctts, 8)? {
            let repeat = u32_at(ctts, 8 + i * 8)? as usize;
            let raw = u32_at(ctts, 12 + i * 8)?;
            let value = if ctts[0] == 1 { raw as i32 as i64 } else { raw as i64 };
            if repeat > n - index {
                return Err("invalid composition time table");
            }
            offsets[index..index + repeat].fill(value);
            index += repeat;
        }
        if index != n {
            return Err("incomplete composition time table");
        }
    }
    let mut sync = alloc::vec![true; n];
    if let Some(stss) = optional(stbl, b"stss")? {
        sync.fill(false);
        for i in 0..count(stss, 4)? {
            let entry = u32_at(stss, 8 + i * 4)? as usize;
            if entry == 0 || entry > n {
                return Err("invalid sync sample index");
            }
            sync[entry - 1] = true;
        }
    }
    let stsc = child(stbl, b"stsc")?;
    let entries = count(stsc, 12)?;
    if entries == 0 || u32_at(stsc, 8)? != 1 {
        return Err("invalid sample-to-chunk table");
    }
    let mut chunks = Vec::new();
    if let Some(stco) = optional(stbl, b"stco")? {
        for i in 0..count(stco, 4)? {
            chunks.push(u32_at(stco, 8 + i * 4)? as u64);
        }
    } else {
        let co64 = child(stbl, b"co64")?;
        for i in 0..count(co64, 8)? {
            chunks.push(u64_at(co64, 8 + i * 8)?);
        }
    }
    for i in 0..entries {
        let first = u32_at(stsc, 8 + i * 12)? as usize;
        if first == 0
            || first > chunks.len()
            || u32_at(stsc, 16 + i * 12)? != 1
            || (i > 0 && first <= u32_at(stsc, 8 + (i - 1) * 12)? as usize)
        {
            return Err("invalid sample-to-chunk entry");
        }
    }
    let mut samples = Vec::with_capacity(n);
    let mut entry = 0;
    let mut sample_index = 0;
    for (i, &chunk) in chunks.iter().enumerate() {
        if entry + 1 < entries && i + 1 == u32_at(stsc, 8 + (entry + 1) * 12)? as usize {
            entry += 1;
        }
        let amount = u32_at(stsc, 12 + entry * 12)? as usize;
        if amount == 0 || amount > n - sample_index {
            return Err("invalid samples per chunk");
        }
        let mut at = usize::try_from(chunk).map_err(|_| "chunk offset too large")?;
        for _ in 0..amount {
            let size = sizes[sample_index];
            let end = at.checked_add(size).ok_or("sample offset overflow")?;
            if end > file.len() {
                return Err("video sample outside file");
            }
            let (time, delta) = times[sample_index];
            let pts = (time as i128 + offsets[sample_index] as i128).max(0) as u64;
            samples.push(Sample {
                offset: at,
                size,
                time_ms: pts.checked_mul(1000).ok_or("timestamp overflow")? / scale,
                duration_ms: delta * 1000 / scale,
                sync: sync[sample_index],
                trim_ms: 0,
            });
            sample_index += 1;
            at = end;
        }
    }
    if sample_index != n {
        return Err("incomplete sample-to-chunk table");
    }
    Ok((duration.checked_mul(1000).ok_or("duration overflow")? / scale, samples))
}
