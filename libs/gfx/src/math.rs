//! Integer math helpers for the GUI (the kernel is built soft-float).

include!(concat!(env!("OUT_DIR"), "/sine.rs"));

/// sin of `phase` (1024 units per turn), scaled by 16384.
#[inline]
pub fn sin(phase: i32) -> i32 {
    SINE[(phase & 1023) as usize] as i32
}

/// cos of `phase` (1024 units per turn), scaled by 16384.
#[inline]
pub fn cos(phase: i32) -> i32 {
    sin(phase + 256)
}

/// The angle of (`x`, `y`) in 1024 units per turn, 0 along +x and growing
/// towards +y (clockwise on screen). Accurate to about a third of a degree.
pub fn atan2(y: i64, x: i64) -> i32 {
    if x == 0 && y == 0 {
        return 0;
    }
    let (ax, ay) = (x.abs(), y.abs());
    let (lo, hi) = if ax >= ay { (ay, ax) } else { (ax, ay) };
    let t = (lo << 12) / hi; // tan of the angle within the octant, 1/4096
    let t2 = t * t >> 12;
    // atan(t) ≈ t·(0.9724 − 0.1919·t²) radians, converted to 1024 units per turn.
    let mut a = (t * (158_470 - 31_270 * t2 / 4096) / 1000) >> 12;
    if ax < ay {
        a = 256 - a;
    }
    if x < 0 {
        a = 512 - a;
    }
    if y < 0 {
        a = 1024 - a;
    }
    (a & 1023) as i32
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atan2_matches_the_compass() {
        for (y, x, want) in [(0, 10, 0), (10, 10, 128), (10, 0, 256), (10, -10, 384), (0, -10, 512), (-10, -10, 640), (-10, 0, 768), (-10, 10, 896)] {
            let got = atan2(y, x);
            let err = (got - want).rem_euclid(1024).min((want - got).rem_euclid(1024));
            assert!(err <= 1, "atan2({y}, {x}) = {got}, want {want}");
        }
        // Agrees with the sine table within a unit all the way round.
        for a in (0..1024).step_by(7) {
            let got = atan2(sin(a) as i64, cos(a) as i64);
            let err = (got - a).rem_euclid(1024).min((a - got).rem_euclid(1024));
            assert!(err <= 2, "angle {a} came back as {got}");
        }
    }
}

/// xorshift PRNG for deterministic procedural art.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn range(&mut self, n: u32) -> i32 {
        (self.next_u64() % n as u64) as i32
    }
}
