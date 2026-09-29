//! Integer math helpers for the GUI (the kernel is built soft-float).

include!(concat!(env!("OUT_DIR"), "/sine.rs"));

/// sin of `phase` (1024 units per turn), scaled by 16384.
#[inline]
pub fn sin(phase: i32) -> i32 {
    SINE[(phase & 1023) as usize] as i32
}

/// Integer square root (floor), Newton's method from above.
pub fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let mut x = 1u64 << (64 - n.leading_zeros()).div_ceil(2);
    loop {
        let y = (x + n / x) / 2;
        if y >= x {
            return x;
        }
        x = y;
    }
}

/// xorshift PRNG for deterministic procedural art.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn range(&mut self, n: u32) -> i32 {
        (self.next() % n as u64) as i32
    }
}
