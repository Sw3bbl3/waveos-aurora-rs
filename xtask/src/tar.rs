//! Minimal ustar writer for the system image (`/System`). Its headers come
//! from `gina::system::tar_header`, the same ones a device writes when it
//! rebuilds the image from a system update's files.

use std::io;
use std::path::Path;

pub struct Tar {
    out: Vec<u8>,
}

impl Tar {
    pub fn new() -> Self {
        Self { out: Vec::new() }
    }

    pub fn dir(&mut self, name: &str) {
        let name = name.trim_end_matches('/');
        assert!(name.len() < 99, "tar path too long: {name}");
        self.out.extend_from_slice(&gina::system::tar_header(name, 0, true));
    }

    pub fn file(&mut self, name: &str, data: &[u8]) {
        assert!(name.len() < 99, "tar path too long: {name}");
        self.out.extend_from_slice(&gina::system::tar_header(name, data.len() as u64, false));
        self.out.extend_from_slice(data);
        self.out.extend(std::iter::repeat_n(0u8, gina::system::padding(data.len() as u64)));
    }

    pub fn finish(mut self, path: &Path) -> io::Result<()> {
        self.out.extend(std::iter::repeat_n(0u8, 1024));
        std::fs::write(path, self.out)
    }
}
