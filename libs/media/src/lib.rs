//! Native, no_std media decoding. No host processes, system codecs or FFI.
#![no_std]
extern crate alloc;

pub mod fmp4;
pub mod mp4;

use alloc::vec::Vec;
use rusty_h264_common::{nal::emulation_unprevent, YuvFrame};
use rusty_h264_decoder::{Decoder, Sps};

pub type Result<T> = core::result::Result<T, &'static str>;
pub const MAX_WIDTH: usize = 1920;
pub const MAX_HEIGHT: usize = 1088;

/// AAC-LC mono/stereo, resampled to WaveOS's 48 kHz stereo output.
pub struct AudioDecoder {
    decoder: rusty_aac::AacDecoder,
    phase: u64,
}
impl AudioDecoder {
    pub fn new(config: &[u8]) -> Result<Self> {
        let c = rusty_aac::parse_audio_specific_config(config).map_err(|_| "invalid AAC configuration")?;
        if c.object_type != 2 || !(1..=2).contains(&c.channels) || !(8000..=48000).contains(&c.sample_rate) {
            return Err("only mono/stereo AAC-LC up to 48 kHz is supported");
        }
        let decoder = rusty_aac::AacDecoder::with_config_bytes(config).map_err(|_| "unsupported AAC configuration")?;
        Ok(Self { decoder, phase: 0 })
    }
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>> {
        if packet.len() > 65536 {
            return Err("AAC packet too large");
        }
        let pcm = self.decoder.decode(packet, None).map_err(|_| "corrupt AAC packet")?;
        if !(1..=2).contains(&pcm.channels) || !(8000..=48000).contains(&pcm.sample_rate) {
            return Err("unsupported AAC output format");
        }
        let frames = pcm.frames() as u64;
        let mut out = Vec::new();
        while self.phase < frames * 48000 {
            let index = (self.phase / 48000) as usize;
            let frac = (self.phase % 48000) as f32 / 48000.0;
            let next = (index + 1).min(frames.saturating_sub(1) as usize);
            for ch in 0..2 {
                let ch = ch.min(pcm.channels as usize - 1);
                let a = pcm.samples[index * pcm.channels as usize + ch];
                let b = pcm.samples[next * pcm.channels as usize + ch];
                out.push(((a + (b - a) * frac).clamp(-1.0, 1.0) * 32767.0) as i16);
            }
            self.phase += pcm.sample_rate as u64;
        }
        self.phase -= frames * 48000;
        Ok(out)
    }
}

pub struct VideoDecoder {
    decoder: Decoder,
    nal_length: usize,
    packet: Vec<u8>,
}

impl VideoDecoder {
    pub fn new(config: &[u8]) -> Result<Self> {
        if config.len() < 7 || config[0] != 1 {
            return Err("invalid AVC configuration");
        }
        let nal_length = (config[4] & 3) as usize + 1;
        if nal_length == 3 {
            return Err("invalid AVC NAL length");
        }
        let mut init = Vec::new();
        let mut at = 6;
        let sps_count = config[5] & 31;
        if sps_count == 0 {
            return Err("missing AVC sequence parameters");
        }
        for _ in 0..sps_count {
            config_nal(config, &mut at, &mut init)?;
        }
        let count = *config.get(at).ok_or("missing AVC picture parameters")?;
        at += 1;
        if count == 0 {
            return Err("missing AVC picture parameters");
        }
        for _ in 0..count {
            config_nal(config, &mut at, &mut init)?;
        }
        let mut decoder = Decoder::new();
        decoder.decode(&init).map_err(|_| "unsupported AVC parameters")?;
        Ok(Self { decoder, nal_length, packet: Vec::new() })
    }

    pub fn decode(&mut self, sample: &[u8]) -> Result<Option<YuvFrame>> {
        if sample.len() > 8 << 20 {
            return Err("video sample too large");
        }
        self.packet.clear();
        let mut at = 0;
        while at < sample.len() {
            let bytes = sample.get(at..at + self.nal_length).ok_or("truncated NAL length")?;
            let len = bytes.iter().fold(0usize, |n, &b| (n << 8) | b as usize);
            at += self.nal_length;
            let end = at.checked_add(len).ok_or("invalid NAL length")?;
            let nal = sample.get(at..end).ok_or("truncated NAL unit")?;
            validate_nal(nal)?;
            self.packet.extend_from_slice(&[0, 0, 0, 1]);
            self.packet.extend_from_slice(nal);
            at = end;
        }
        self.decoder.decode(&self.packet).map_err(|_| "unsupported or corrupt H.264 video")
    }
}

fn config_nal(data: &[u8], at: &mut usize, out: &mut Vec<u8>) -> Result<()> {
    let len = data.get(*at..*at + 2).ok_or("truncated AVC configuration")?;
    let len = u16::from_be_bytes([len[0], len[1]]) as usize;
    *at += 2;
    let nal = data.get(*at..*at + len).ok_or("truncated AVC configuration")?;
    validate_nal(nal)?;
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
    *at += len;
    Ok(())
}

fn validate_nal(nal: &[u8]) -> Result<()> {
    let header = *nal.first().ok_or("empty NAL unit")?;
    if header & 0x80 != 0 {
        return Err("invalid NAL header");
    }
    if header & 31 == 7 {
        let rbsp = emulation_unprevent(&nal[1..]);
        let sps = Sps::parse(&rbsp).map_err(|_| "unsupported H.264 sequence")?;
        if sps.coded_width() > MAX_WIDTH || sps.coded_height() > MAX_HEIGHT || sps.max_num_ref_frames > 16 {
            return Err("video exceeds decoder limits (1920 by 1080)");
        }
    }
    Ok(())
}

/// [`pixels`] scaled to `dw` × `dh` in the same pass (bilinear luma,
/// nearest chroma; integer arithmetic only).
pub fn pixels_scaled(frame: &YuvFrame, dw: usize, dh: usize) -> Result<Vec<u32>> {
    let (w, h) = (frame.width, frame.height);
    if dw == w && dh == h {
        return pixels(frame);
    }
    if w == 0
        || h == 0
        || dw == 0
        || dh == 0
        || dw > 4096
        || dh > 4096
        || frame.y.len() < w * h
        || frame.u.len() < w * h / 4
        || frame.v.len() < w * h / 4
    {
        return Err("invalid decoded frame dimensions");
    }
    let step_x = ((w as u64) << 16) / dw as u64;
    let step_y = ((h as u64) << 16) / dh as u64;
    let mut xs = Vec::with_capacity(dw);
    for x in 0..dw {
        let fx = (x as u64 * step_x + step_x / 2).saturating_sub(32768);
        let x0 = ((fx >> 16) as usize).min(w - 1);
        xs.push((x0, (x0 + 1).min(w - 1), ((fx & 0xFFFF) >> 8) as i32));
    }
    let mut out = Vec::with_capacity(dw * dh);
    for y in 0..dh {
        let fy = (y as u64 * step_y + step_y / 2).saturating_sub(32768);
        let y0 = ((fy >> 16) as usize).min(h - 1);
        let y1 = (y0 + 1).min(h - 1);
        let ty = ((fy & 0xFFFF) >> 8) as i32;
        let (r0, r1) = (&frame.y[y0 * w..y0 * w + w], &frame.y[y1 * w..y1 * w + w]);
        let crow = (y0 / 2) * (w / 2);
        for &(x0, x1, tx) in &xs {
            let top = r0[x0] as i32 * (256 - tx) + r0[x1] as i32 * tx;
            let bottom = r1[x0] as i32 * (256 - tx) + r1[x1] as i32 * tx;
            let luma = (top * (256 - ty) + bottom * ty) >> 16;
            let uv = crow + x0 / 2;
            let c = luma - 16;
            let d = frame.u[uv] as i32 - 128;
            let e = frame.v[uv] as i32 - 128;
            let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u32;
            let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u32;
            let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u32;
            out.push(0xff00_0000 | r << 16 | g << 8 | b);
        }
    }
    Ok(out)
}

/// BT.601 limited-range YUV420 to the desktop's ARGB pixels.
pub fn pixels(frame: &YuvFrame) -> Result<Vec<u32>> {
    let (w, h) = (frame.width, frame.height);
    if w == 0
        || h == 0
        || w > MAX_WIDTH
        || h > MAX_HEIGHT
        || w % 2 != 0
        || h % 2 != 0
        || frame.y.len() < w * h
        || frame.u.len() < w * h / 4
        || frame.v.len() < w * h / 4
    {
        return Err("invalid decoded frame dimensions");
    }
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let c = frame.y[y * w + x] as i32 - 16;
            let uv = (y / 2) * (w / 2) + x / 2;
            let d = frame.u[uv] as i32 - 128;
            let e = frame.v[uv] as i32 - 128;
            let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u32;
            let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u32;
            let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u32;
            out.push(0xff00_0000 | r << 16 | g << 8 | b);
        }
    }
    Ok(out)
}
