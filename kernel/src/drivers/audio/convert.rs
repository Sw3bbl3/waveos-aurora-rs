//! Sample format and rate conversion between the mixer (48 kHz, 16-bit
//! stereo) and what a device speaks.
//!
//! PCM on USB and HDA is little-endian, signed, and MSB-justified in its
//! subslot (a 24-bit sample in 4 bytes occupies the top three).

/// A device's native stream format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u8,
    /// Bytes per sample on the wire (2, 3 or 4).
    pub subslot: u8,
    /// Significant bits (16, 24, 32).
    pub bits: u8,
}

impl Format {
    pub const MIXER: Format = Format { rate: super::RATE as u32, channels: 2, subslot: 2, bits: 16 };

    pub fn frame_bytes(&self) -> usize {
        self.channels as usize * self.subslot as usize
    }
}

/// Writes a stereo frame in `f`'s layout to `out` (one frame long). Extra
/// channels are silent; a mono device gets the average.
pub fn pack(frame: [i16; 2], f: &Format, out: &mut [u8]) {
    let n = f.subslot as usize;
    for ch in 0..f.channels as usize {
        let v = match (f.channels, ch) {
            (1, _) => ((frame[0] as i32 + frame[1] as i32) / 2) as i16,
            (_, 0 | 1) => frame[ch],
            _ => 0,
        };
        let wide = (v as i32) << (8 * n - 16).min(16);
        let bytes = wide.to_le_bytes();
        out[ch * n..ch * n + n].copy_from_slice(&bytes[..n]);
    }
}

/// Reads one frame in `f`'s layout as a stereo frame.
pub fn unpack(data: &[u8], f: &Format) -> [i16; 2] {
    let n = f.subslot as usize;
    let sample = |ch: usize| -> i16 {
        let mut b = [0u8; 4];
        b[4 - n..].copy_from_slice(&data[ch * n..ch * n + n]);
        (i32::from_le_bytes(b) >> 16) as i16
    };
    if f.channels == 1 {
        let s = sample(0);
        [s, s]
    } else {
        [sample(0), sample(1)]
    }
}

/// Streaming linear-interpolation resampler for stereo frames.
pub struct Resampler {
    /// Input frames per output frame, 16.16.
    step: u32,
    /// Position of the next output frame past `prev`, 16.16.
    pos: u32,
    prev: [i16; 2],
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Resampler {
        Resampler { step: ((from as u64) << 16).div_euclid(to.max(1) as u64) as u32, pos: 0, prev: [0; 2] }
    }

    /// Feeds one input frame; appends the output frames it completes.
    pub fn push(&mut self, cur: [i16; 2], out: &mut alloc::vec::Vec<i16>) {
        while self.pos < 0x1_0000 {
            for ch in 0..2 {
                let (a, b) = (self.prev[ch] as i32, cur[ch] as i32);
                out.push((a + (((b - a) * self.pos as i32) >> 16)) as i16);
            }
            self.pos += self.step.max(1);
        }
        self.pos -= 0x1_0000;
        self.prev = cur;
    }
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    // Round trips through every layout.
    for f in [
        Format { rate: 48_000, channels: 2, subslot: 2, bits: 16 },
        Format { rate: 48_000, channels: 2, subslot: 3, bits: 24 },
        Format { rate: 48_000, channels: 4, subslot: 4, bits: 24 },
    ] {
        let mut b = [0u8; 16];
        pack([1234, -5678], &f, &mut b);
        assert_eq!(unpack(&b, &f), [1234, -5678], "{:?}", f);
    }
    let mono = Format { rate: 48_000, channels: 1, subslot: 2, bits: 16 };
    let mut b = [0u8; 2];
    pack([1000, 3000], &mono, &mut b);
    assert_eq!(unpack(&b, &mono), [2000, 2000]);
    // 44.1 kHz → 48 kHz makes 480 frames of 441, and keeps a ramp a ramp.
    let mut r = Resampler::new(44_100, 48_000);
    let mut out = alloc::vec::Vec::new();
    for i in 0..441 {
        r.push([i * 10, -i * 10], &mut out);
    }
    let frames = out.len() / 2;
    assert!((479..=481).contains(&frames), "{} frames", frames);
    assert!(out.chunks(2).zip(out.chunks(2).skip(1)).all(|(a, b)| b[0] >= a[0] && b[1] <= a[1]));
}
