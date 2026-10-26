//! Deterministic input mutation for robustness tests.
//!
//! Everything that reads bytes from outside — pictures, fonts, web pages,
//! certificates, disks — must return an error for bad input, never panic or
//! hang: in the kernel a panic stops the machine, and in an app it loses the
//! user's work. These tests feed each parser thousands of corrupted copies of
//! valid inputs (bit flips, boundary values, truncation, duplicated and deleted
//! runs, and for text, splices of tokens that stress the grammar).
//!
//! Runs are reproducible: the same seed gives the same inputs, and a failure
//! names the iteration. `AURORA_MUTATIONS=N` multiplies every run's length for
//! a deeper search than CI's.
//!
//! ```
//! aurora_mutate::check("bytes", &[b"seed".to_vec()], 100, |input| {
//!     let _ = core::str::from_utf8(input);
//! });
//! ```

use std::panic::{catch_unwind, AssertUnwindSafe};

/// An xorshift64 generator and the mutations it drives.
pub struct Mutator(u64);

/// Bytes that sit on boundaries parsers get wrong.
const BYTES: [u8; 10] = [0x00, 0x01, 0x02, 0x10, 0x20, 0x40, 0x7F, 0x80, 0xFE, 0xFF];

/// The same for 32-bit fields (lengths, counts, offsets).
const WORDS: [u32; 8] = [0, 1, 0x7FFF, 0xFFFF, 0x1_0000, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF];

impl Mutator {
    pub fn new(seed: u64) -> Mutator {
        Mutator(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number in `0..n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// One to eight random edits anywhere in `seed`, which may change its length.
    pub fn mutate(&mut self, seed: &[u8]) -> Vec<u8> {
        let mut d = seed.to_vec();
        for _ in 0..1 + self.below(8) {
            if d.is_empty() {
                d.push(self.next_u64() as u8);
                continue;
            }
            let at = self.below(d.len());
            match self.below(8) {
                0..=3 => self.overwrite(&mut d, at),
                4 => d.truncate(at),
                5 => {
                    let run = d[at..][..self.below(64).min(d.len() - at)].to_vec();
                    let to = self.below(d.len());
                    d.splice(to..to, run);
                }
                6 => {
                    let n = self.below(32).min(d.len() - at);
                    d.drain(at..at + n);
                }
                _ => d.insert(at, self.next_u64() as u8),
            }
        }
        d
    }

    /// Like [`Mutator::mutate`], but only overwrites bytes inside `ranges`
    /// and keeps the length: for images of block devices, where the
    /// interesting bytes are a few metadata blocks among megabytes of zeros.
    pub fn mutate_within(&mut self, seed: &[u8], ranges: &[core::ops::Range<usize>]) -> Vec<u8> {
        let mut d = seed.to_vec();
        for _ in 0..1 + self.below(8) {
            let r = &ranges[self.below(ranges.len())];
            let at = r.start + self.below(r.len());
            let at = at.min(d.len() - 1);
            self.overwrite(&mut d, at);
        }
        d
    }

    /// Inserts up to five of `words` at random places in `input`.
    pub fn splice_words(&mut self, input: &mut Vec<u8>, words: &[&str]) {
        for _ in 0..self.below(6) {
            let at = self.below(input.len() + 1);
            let w = words[self.below(words.len())];
            input.splice(at..at, w.bytes());
        }
    }

    fn overwrite(&mut self, d: &mut [u8], at: usize) {
        match self.below(4) {
            0 => d[at] ^= 1 << self.below(8),
            1 => d[at] = BYTES[self.below(BYTES.len())],
            2 => d[at] = self.next_u64() as u8,
            _ => {
                let word = WORDS[self.below(WORDS.len())];
                let bytes = if self.below(2) == 0 { word.to_le_bytes() } else { word.to_be_bytes() };
                for (b, v) in d[at..].iter_mut().zip(bytes) {
                    *b = v;
                }
            }
        }
    }
}

/// How many times longer than asked to run (`AURORA_MUTATIONS`, default 1).
pub fn scale() -> usize {
    std::env::var("AURORA_MUTATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(1).max(1)
}

/// The ranges of `unit`-sized blocks of `image` that hold anything but zeros.
pub fn used_blocks(image: &[u8], unit: usize) -> Vec<core::ops::Range<usize>> {
    image
        .chunks(unit)
        .enumerate()
        .filter(|(_, block)| block.iter().any(|&b| b != 0))
        .map(|(i, block)| i * unit..i * unit + block.len())
        .collect()
}

/// Runs `f` on `iterations` (times [`scale`]) mutations of `seeds`, failing
/// with the iteration that panicked. `name` seeds the generator.
pub fn check(name: &str, seeds: &[Vec<u8>], iterations: usize, f: impl Fn(&[u8])) {
    check_with(name, seeds, iterations, |m, seed| m.mutate(seed), f);
}

/// [`check`] with a custom mutation (for example [`Mutator::mutate_within`]
/// or one that also calls [`Mutator::splice_words`]).
pub fn check_with(
    name: &str,
    seeds: &[Vec<u8>],
    iterations: usize,
    mutate: impl Fn(&mut Mutator, &[u8]) -> Vec<u8>,
    f: impl Fn(&[u8]),
) {
    assert!(!seeds.is_empty(), "{name}: no seeds");
    let mut m =
        Mutator::new(name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)));
    for i in 0..iterations * scale() {
        let seed = &seeds[m.below(seeds.len())];
        let input = mutate(&mut m, seed);
        if let Err(e) = catch_unwind(AssertUnwindSafe(|| f(&input))) {
            let msg = e
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            panic!("{name}: mutation {i} ({} bytes) panicked: {msg}", input.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let (mut a, mut b) = (Mutator::new(7), Mutator::new(7));
        for _ in 0..100 {
            assert_eq!(a.mutate(b"hello, world"), b.mutate(b"hello, world"));
        }
    }

    #[test]
    fn within_keeps_length_and_ranges() {
        let image = [0u8; 4096];
        let mut m = Mutator::new(1);
        for _ in 0..100 {
            let out = m.mutate_within(&image, core::slice::from_ref(&(512..1024)));
            assert_eq!(out.len(), image.len());
            assert!(out[..512].iter().chain(&out[1024 + 3..]).all(|&b| b == 0));
        }
    }

    #[test]
    fn used_blocks_finds_data() {
        let mut image = vec![0u8; 2048];
        image[600] = 1;
        assert_eq!(used_blocks(&image, 512), core::slice::from_ref(&(512..1024)));
    }

    #[test]
    #[should_panic(expected = "mutation")]
    fn reports_panics() {
        check("panics", &[vec![1, 2, 3]], 100, |d| assert!(d.len() < 4));
    }
}
