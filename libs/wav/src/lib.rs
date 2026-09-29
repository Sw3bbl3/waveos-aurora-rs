//! WAV (RIFF PCM) reading and writing, integer-only so the soft-float kernel
//! can use it. Decoding converts any 8/16-bit mono/stereo file to the
//! system's playback format: interleaved stereo `i16` at a chosen rate.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// A parsed PCM WAV file.
pub struct Wav<'a> {
    pub channels: usize,
    pub rate: usize,
    pub bits: usize,
    pub data: &'a [u8],
}

impl Wav<'_> {
    pub fn frames(&self) -> usize {
        self.data.len() / (self.channels * self.bits / 8)
    }

    /// Duration in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.frames() as u64 * 1000 / self.rate as u64
    }

    fn sample(&self, frame: usize, channel: usize) -> i32 {
        let o = (frame * self.channels + channel.min(self.channels - 1)) * self.bits / 8;
        if self.bits == 8 {
            (self.data[o] as i32 - 128) << 8
        } else {
            i16::from_le_bytes([self.data[o], self.data[o + 1]]) as i32
        }
    }
}

/// Parses the RIFF structure; `None` unless it is integer PCM we can play.
pub fn parse(b: &[u8]) -> Option<Wav<'_>> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let (mut channels, mut rate, mut bits, mut data) = (0, 0, 0, None);
    let mut off = 12;
    while off + 8 <= b.len() {
        let len = u32_at(off + 4) as usize;
        let body = off + 8;
        let end = body.saturating_add(len).min(b.len());
        match &b[off..off + 4] {
            b"fmt " if len >= 16 && end >= body + 16 => {
                // Format 1 is integer PCM (0xFFFE, "extensible", is accepted as PCM too).
                if !matches!(u16_at(body), 1 | 0xFFFE) {
                    return None;
                }
                channels = u16_at(body + 2) as usize;
                rate = u32_at(body + 4) as usize;
                bits = u16_at(body + 14) as usize;
            }
            b"data" => data = Some(&b[body..end]),
            _ => {}
        }
        off = body.saturating_add(len).saturating_add(len & 1);
    }
    let data = data?;
    let ok = (1..=2).contains(&channels) && (4000..=192_000).contains(&rate) && (bits == 8 || bits == 16);
    ok.then_some(Wav { channels, rate, bits, data })
}

/// Decodes to interleaved stereo `i16` at `rate` (linear resampling).
pub fn decode(b: &[u8], rate: usize) -> Option<Vec<i16>> {
    let w = parse(b)?;
    let frames = w.frames();
    if frames == 0 {
        return Some(Vec::new());
    }
    let out_frames = (frames as u64 * rate as u64 / w.rate as u64) as usize;
    // Source position per output frame, in 16.16 fixed point.
    let step = ((w.rate as u64) << 16) / rate as u64;
    let mut out = Vec::with_capacity(out_frames * 2);
    for i in 0..out_frames {
        let pos = i as u64 * step;
        let f = ((pos >> 16) as usize).min(frames - 1);
        let t = (pos & 0xFFFF) as i32;
        let g = (f + 1).min(frames - 1);
        for c in 0..2 {
            let (a, b) = (w.sample(f, c), w.sample(g, c));
            out.push((a + (((b - a) * t) >> 16)) as i16);
        }
    }
    Some(out)
}

/// Encodes interleaved `i16` samples as a 16-bit PCM WAV file.
pub fn encode(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    v.extend_from_slice(&(channels * 2).to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn round_trip_stereo() {
        let samples: Vec<i16> = (0..2000).map(|i| (i * 13 % 30000) as i16 - 15000).collect();
        let file = encode(&samples, 2, 48_000);
        assert_eq!(decode(&file, 48_000).unwrap(), samples);
    }

    #[test]
    fn mono_is_duplicated() {
        let file = encode(&[100, -200, 300], 1, 48_000);
        assert_eq!(decode(&file, 48_000).unwrap(), vec![100, 100, -200, -200, 300, 300]);
    }

    #[test]
    fn resamples_to_the_output_rate() {
        let samples = vec![1000i16; 22_050];
        let file = encode(&samples, 1, 22_050);
        let out = decode(&file, 48_000).unwrap();
        assert_eq!(out.len(), 48_000 * 2);
        assert!(out.iter().all(|&s| s == 1000));
        assert_eq!(parse(&file).unwrap().duration_ms(), 1000);
    }

    #[test]
    fn eight_bit_is_widened() {
        let mut file = encode(&[], 1, 8000);
        // Rewrite as an 8-bit file with two samples.
        file[34] = 8;
        file[40..44].copy_from_slice(&2u32.to_le_bytes());
        file.extend_from_slice(&[0, 255]);
        assert_eq!(decode(&file, 8000).unwrap(), vec![-32768, -32768, 32512, 32512]);
    }

    #[test]
    fn rejects_non_pcm() {
        let mut file = encode(&[1, 2], 1, 8000);
        file[20] = 3; // IEEE float
        assert!(parse(&file).is_none());
        assert!(parse(b"RIFF\0\0\0\0WAVE").is_none());
    }
}
