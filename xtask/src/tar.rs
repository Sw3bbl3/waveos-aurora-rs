//! Minimal ustar writer for the system image (`/System`).

use std::io;
use std::path::Path;

pub struct Tar {
    out: Vec<u8>,
}

impl Tar {
    pub fn new() -> Self {
        Self { out: Vec::new() }
    }

    fn header(&mut self, name: &str, size: usize, kind: u8, mode: u32) {
        assert!(name.len() < 100, "tar path too long: {name}");
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let octal = |h: &mut [u8], v: u64, width: usize| {
            let s = format!("{:0w$o}", v, w = width - 1);
            h[..s.len()].copy_from_slice(s.as_bytes());
        };
        octal(&mut h[100..108], mode as u64, 8);
        octal(&mut h[108..116], 0, 8);
        octal(&mut h[116..124], 0, 8);
        octal(&mut h[124..136], size as u64, 12);
        // Seconds since 2000 to match the kernel's wall clock epoch; a fixed value keeps builds reproducible.
        octal(&mut h[136..148], 0, 12);
        h[148..156].fill(b' ');
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        let s = format!("{:06o}\0 ", sum);
        h[148..156].copy_from_slice(s.as_bytes());
        self.out.extend_from_slice(&h);
    }

    pub fn dir(&mut self, name: &str) {
        self.header(&format!("{}/", name.trim_end_matches('/')), 0, b'5', 0o755);
    }

    pub fn file(&mut self, name: &str, data: &[u8]) {
        self.header(name, data.len(), b'0', 0o644);
        self.out.extend_from_slice(data);
        let pad = (512 - data.len() % 512) % 512;
        self.out.extend(std::iter::repeat_n(0u8, pad));
    }

    pub fn finish(mut self, path: &Path) -> io::Result<()> {
        self.out.extend(std::iter::repeat_n(0u8, 1024));
        std::fs::write(path, self.out)
    }
}
