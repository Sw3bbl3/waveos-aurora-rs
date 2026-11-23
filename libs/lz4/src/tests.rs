extern crate std;

use super::*;
use std::vec;
use std::vec::Vec;

fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn pack(src: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; max_compressed(src.len())];
    let mut table = [0u16; TABLE];
    let n = compress(src, &mut out, &mut table).expect("a worst-case buffer always fits");
    out.truncate(n);
    out
}

fn round_trip(src: &[u8]) -> usize {
    let packed = pack(src);
    let mut back = vec![0u8; src.len()];
    assert_eq!(decompress(&packed, &mut back), Ok(src.len()), "length of {} bytes", src.len());
    assert!(back == src, "contents of {} bytes", src.len());
    check_format(&packed, src.len());
    packed.len()
}

/// The end-of-block rules other decoders rely on: the last sequence is
/// literals only, at least five of them (when the block is long enough to
/// have a match at all), and no match starts in the last twelve bytes.
fn check_format(block: &[u8], len: usize) {
    let (mut ip, mut op) = (0, 0);
    let ext = |ip: &mut usize, mut n: usize| loop {
        let b = block[*ip];
        *ip += 1;
        n += b as usize;
        if b != 255 {
            return n;
        }
    };
    loop {
        let token = block[ip];
        ip += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            lit = ext(&mut ip, lit);
        }
        ip += lit;
        op += lit;
        if ip == block.len() {
            if len > MF_LIMIT {
                assert!(lit >= LAST_LITERALS, "the block ends with {lit} literals");
            }
            assert_eq!(op, len);
            return;
        }
        assert!(op + MF_LIMIT <= len, "a match starts {} bytes from the end", len - op);
        ip += 2;
        let mut m = (token & 15) as usize;
        if m == 15 {
            m = ext(&mut ip, m);
        }
        op += m + MIN_MATCH;
        assert!(op + LAST_LITERALS <= len, "a match ends {} bytes from the end", len - op);
    }
}

#[test]
fn empty_and_tiny() {
    for len in 0..40 {
        let src: Vec<u8> = (0..len).map(|i| (i * 7) as u8).collect();
        round_trip(&src);
        round_trip(&vec![0u8; len]);
    }
}

#[test]
fn pages_of_every_kind() {
    let mut seed = 0x1234_5678_9abc_def1;
    let zero = vec![0u8; 4096];
    assert!(round_trip(&zero) < 40, "a zero page compresses to almost nothing");
    let mut random = vec![0u8; 4096];
    random.iter_mut().for_each(|b| *b = rng(&mut seed) as u8);
    let n = round_trip(&random);
    assert!(n <= max_compressed(4096) && n > 4096, "random data doesn't shrink ({n})");
    // Text-like data, a repeating structure, a pointer-heavy page, runs.
    let text: Vec<u8> = b"the quick brown fox jumps over the lazy dog; ".iter().cycle().take(4096).copied().collect();
    assert!(round_trip(&text) < 200);
    let structs: Vec<u8> = (0..4096u32).map(|i| if i % 16 < 8 { (i / 16) as u8 } else { 0xAA }).collect();
    assert!(round_trip(&structs) < 3000);
    let mut pointers = vec![0u8; 4096];
    for (i, w) in pointers.chunks_mut(8).enumerate() {
        let p = 0x0000_7fff_1234_0000u64 + (rng(&mut seed) % 64) * 16 + (i as u64 % 3);
        w.copy_from_slice(&p.to_le_bytes());
    }
    assert!(round_trip(&pointers) < 4096);
    let mut runs = vec![0u8; 4096];
    let mut at = 0;
    while at < runs.len() {
        let n = (rng(&mut seed) % 300) as usize + 1;
        let b = rng(&mut seed) as u8;
        runs[at..(at + n).min(4096)].fill(b);
        at += n;
    }
    round_trip(&runs);
}

#[test]
fn random_mixtures() {
    // Literals and matches of every length, overlapping matches, long
    // literal runs (lengths continued past 255), sizes up to the limit.
    let mut seed = 99;
    for round in 0..400 {
        let len = match round % 4 {
            0 => (rng(&mut seed) % 64) as usize,
            1 => (rng(&mut seed) % 4096) as usize,
            2 => 4096,
            _ => (rng(&mut seed) % MAX_INPUT as u64) as usize + 1,
        };
        let mut src = Vec::with_capacity(len);
        while src.len() < len {
            match rng(&mut seed) % 4 {
                0 => {
                    let n = (rng(&mut seed) % 600) as usize;
                    src.extend((0..n).map(|_| rng(&mut seed) as u8));
                }
                1 if !src.is_empty() => {
                    let back = (rng(&mut seed) as usize % src.len()) + 1;
                    let n = (rng(&mut seed) % 700) as usize;
                    for _ in 0..n {
                        src.push(src[src.len() - back]);
                    }
                }
                2 => {
                    let n = (rng(&mut seed) % 2000) as usize;
                    src.extend(core::iter::repeat_n(rng(&mut seed) as u8, n));
                }
                _ => src.push(rng(&mut seed) as u8),
            }
        }
        src.truncate(len);
        round_trip(&src);
    }
}

#[test]
fn too_small_a_buffer_is_refused() {
    let mut seed = 5;
    let random: Vec<u8> = (0..4096).map(|_| rng(&mut seed) as u8).collect();
    let mut out = vec![0u8; 3072];
    let mut table = [0u16; TABLE];
    assert_eq!(compress(&random, &mut out, &mut table), None, "random data never fits in 3/4 of a page");
    let zero = vec![0u8; 4096];
    assert!(compress(&zero, &mut out, &mut table).is_some());
    assert_eq!(compress(&vec![0u8; MAX_INPUT + 1], &mut out, &mut table), None);
}

#[test]
fn decoder_bounds() {
    let mut out = [0u8; 16];
    assert_eq!(decompress(&[], &mut out), Err(Error::Truncated));
    // Literals longer than the block.
    assert_eq!(decompress(&[0x50, 1, 2], &mut out), Err(Error::Truncated));
    // More literals than the output holds.
    let mut long = vec![0xF0u8, 10];
    long.extend([7u8; 25]);
    assert_eq!(decompress(&long, &mut out), Err(Error::OutputFull));
    // An offset of zero, and one before the start.
    assert_eq!(decompress(&[0x10, b'a', 0, 0, 0x00], &mut out), Err(Error::BadOffset));
    assert_eq!(decompress(&[0x10, b'a', 2, 0, 0x00], &mut out), Err(Error::BadOffset));
    // A match running past the output.
    assert_eq!(decompress(&[0x1F, b'a', 1, 0, 40, 0x00], &mut out), Err(Error::OutputFull));
    // An overlapping match: one byte repeated.
    let mut big = [0u8; 64];
    assert_eq!(decompress(&[0x1F, b'z', 1, 0, 20, 0x50, 1, 2, 3, 4, 5], &mut big), Ok(1 + 39 + 5));
    assert!(big[..40].iter().all(|&b| b == b'z'));
    // Length bytes that would overflow a usize never do.
    let mut huge = vec![0xF0u8];
    huge.extend(core::iter::repeat_n(255u8, 100_000));
    assert!(decompress(&huge, &mut big).is_err());
}

/// A block laid out by hand as the specification describes it decodes.
#[test]
fn reference_block() {
    // Token 0x6F: 6 literals "hello ", then a match 6 back of 15 + 4 + 4
    // bytes; token 0x70: the last 7 literals.
    let block = [
        0x6Fu8, b'h', b'e', b'l', b'l', b'o', b' ', 6, 0, 4, 0x70, b' ', b'w', b'o', b'r', b'l', b'd', b'\n',
    ];
    let mut out = [0u8; 64];
    let n = decompress(&block, &mut out).unwrap();
    assert_eq!(&out[..n], b"hello hello hello hello hello world\n");
}

/// Corrupt blocks (mutations of valid ones) never make the decoder panic
/// or touch memory outside its buffers.
#[test]
fn fuzz_decoder() {
    let mut seed = 7;
    let mut seeds = Vec::new();
    for len in [0usize, 13, 100, 1000, 4096] {
        let mut src: Vec<u8> = (0..len).map(|i| if i % 7 == 0 { rng(&mut seed) as u8 } else { (i / 7) as u8 }).collect();
        seeds.push(pack(&src));
        src.iter_mut().for_each(|b| *b = rng(&mut seed) as u8);
        seeds.push(pack(&src));
    }
    seeds.push(pack(&[0u8; 4096]));
    aurora_mutate::check("lz4 block", &seeds, 20_000, |input| {
        for size in [0usize, 7, 4096, 70_000] {
            let mut out = vec![0u8; size];
            if let Ok(n) = decompress(input, &mut out) {
                assert!(n <= size);
            }
        }
    });
}

/// The compressor on mutated inputs: still a round trip.
#[test]
fn fuzz_compressor() {
    let seeds = vec![
        b"the quick brown fox jumps over the lazy dog".repeat(50),
        (0..4096u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>(),
        vec![0u8; 4096],
    ];
    aurora_mutate::check("lz4 round trip", &seeds, 3_000, |input| {
        if input.len() <= MAX_INPUT {
            round_trip(input);
        }
    });
}

/// Interoperability with the reference implementation, where the `lz4`
/// tool is installed (skipped otherwise): it decodes what this writes, and
/// this decodes what it writes, through its legacy frame format (a magic
/// number, then each block after its 32-bit length).
#[test]
fn reference_tool() {
    use std::process::{Command, Stdio};
    if Command::new("lz4").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() {
        std::eprintln!("lz4 not installed: skipping the interoperability check");
        return;
    }
    let dir = std::env::temp_dir().join(std::format!("aurora-lz4-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut seed = 3;
    let mut inputs: Vec<Vec<u8>> = vec![
        b"hello hello hello hello hello world\n".to_vec(),
        b"the quick brown fox jumps over the lazy dog; ".iter().cycle().take(4096).copied().collect(),
        vec![0u8; 4096],
        (0..4096).map(|_| rng(&mut seed) as u8).collect(),
    ];
    let mut runs = Vec::new();
    while runs.len() < 60_000 {
        let n = (rng(&mut seed) % 200) as usize;
        if rng(&mut seed).is_multiple_of(2) {
            runs.extend(core::iter::repeat_n(rng(&mut seed) as u8, n));
        } else {
            runs.extend((0..n).map(|_| rng(&mut seed) as u8));
        }
    }
    inputs.push(runs);
    for (i, src) in inputs.iter().enumerate() {
        // Ours, decoded by the tool.
        let block = pack(src);
        let mut file = 0x184C_2102u32.to_le_bytes().to_vec();
        file.extend((block.len() as u32).to_le_bytes());
        file.extend(&block);
        let ours = dir.join(std::format!("ours{i}.lz4"));
        std::fs::write(&ours, &file).unwrap();
        let out = Command::new("lz4").args(["-d", "-c"]).arg(&ours).output().unwrap();
        let err = std::string::String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "lz4 rejected our block {i}: {err}");
        assert!(out.stdout == *src, "lz4 decoded our block {i} differently");
        // Theirs, decoded here.
        let plain = dir.join(std::format!("plain{i}"));
        std::fs::write(&plain, src).unwrap();
        let out = Command::new("lz4").args(["-l", "-c", "-9"]).arg(&plain).output().unwrap();
        assert!(out.status.success());
        let f = &out.stdout;
        assert_eq!(u32::from_le_bytes(f[..4].try_into().unwrap()), 0x184C_2102);
        let n = u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize;
        let mut back = vec![0u8; src.len()];
        assert_eq!(decompress(&f[8..8 + n], &mut back), Ok(src.len()), "their block {i}");
        assert!(back == *src);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
