//! Random numbers: a ChaCha20 generator seeded (and periodically reseeded)
//! from the CPU's RDSEED/RDRAND when present, mixed with timer jitter.
//!
//! Used for TCP sequence numbers, DHCP and DNS ids, ephemeral ports, and
//! handed to programs through `getrandom` (TLS keys).

use crate::sync::IrqMutex;

struct Generator {
    key: [u32; 8],
    counter: u64,
    /// Bytes produced since the last reseed.
    produced: u64,
    seeded: bool,
}

static GEN: IrqMutex<Generator> = IrqMutex::new(Generator { key: [0; 8], counter: 0, produced: 0, seeded: false });

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One ChaCha20 block (RFC 8439) with a 64-bit counter and zero nonce.
fn block(key: &[u32; 8], counter: u64) -> [u32; 16] {
    let mut s = [0u32; 16];
    s[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    s[4..12].copy_from_slice(key);
    s[12] = counter as u32;
    s[13] = (counter >> 32) as u32;
    let init = s;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    for (x, i) in s.iter_mut().zip(init) {
        *x = x.wrapping_add(i);
    }
    s
}

/// 64 bits of entropy-ish input: hardware randomness when available, and
/// the low bits of timer readings (which jitter with interrupts and caches).
fn entropy() -> u64 {
    let mut v = crate::arch::rng::seed().or_else(crate::arch::rng::random).unwrap_or(0);
    for i in 0..8 {
        let t = crate::arch::cpu::cycles() ^ crate::time::now_ns().rotate_left(i * 7);
        v = v.rotate_left(13) ^ t.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    v
}

fn reseed(g: &mut Generator) {
    // New key = old key XOR fresh input, stirred through one block.
    for k in g.key.iter_mut() {
        *k ^= entropy() as u32;
    }
    let b = block(&g.key, g.counter ^ 0xFFFF_FFFF_0000_0000);
    g.key.copy_from_slice(&b[..8]);
    g.produced = 0;
    g.seeded = true;
}

/// Fills `out` with random bytes.
pub fn fill(out: &mut [u8]) {
    let mut g = GEN.lock();
    if !g.seeded || g.produced > 1 << 20 {
        reseed(&mut g);
    }
    for chunk in out.chunks_mut(64) {
        let b = block(&g.key, g.counter);
        g.counter = g.counter.wrapping_add(1);
        for (i, byte) in chunk.iter_mut().enumerate() {
            *byte = (b[i / 4] >> (8 * (i % 4))) as u8;
        }
    }
    g.produced += out.len() as u64;
    // Forward secrecy: rekey from the generator's own output.
    let next = block(&g.key, g.counter);
    g.counter = g.counter.wrapping_add(1);
    g.key.copy_from_slice(&next[..8]);
}

pub fn u32() -> u32 {
    let mut b = [0u8; 4];
    fill(&mut b);
    u32::from_le_bytes(b)
}

pub fn u64() -> u64 {
    let mut b = [0u8; 8];
    fill(&mut b);
    u64::from_le_bytes(b)
}

/// One ChaCha20 block for `key` at `counter` (the swap file's cipher).
pub fn chacha_block(key: &[u32; 8], counter: u64) -> [u32; 16] {
    block(key, counter)
}

/// Whether the CPU provides hardware randomness (otherwise only timer jitter).
pub fn hardware() -> bool {
    crate::arch::rng::random().is_some()
}

#[cfg(feature = "ktest")]
pub fn chacha_block_for_test(key: &[u32; 8], counter: u64) -> [u32; 16] {
    block(key, counter)
}
