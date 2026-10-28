//! Reading ZIP archives: the list of entries from the central directory,
//! and each entry's bytes (stored, or deflated), checked against its CRC-32.
//! ZIP64 and encrypted archives are not supported.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a ZIP archive, or a damaged one.
    Damaged,
    /// A feature this reader doesn't have (ZIP64, encryption, a method other than deflate).
    Unsupported,
    /// The entry's contents don't match its checksum.
    Checksum,
    TooLarge,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str(match self {
            Error::Damaged => "it is damaged",
            Error::Unsupported => "it uses a ZIP feature WaveOS can't read",
            Error::Checksum => "its contents don't match their checksum",
            Error::TooLarge => "it is too large",
        })
    }
}

/// One file or folder in an archive.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub compressed: u64,
    method: u16,
    flags: u16,
    crc: u32,
    /// Offset of the local header.
    local: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
    /// The last part of the name.
    pub fn file_name(&self) -> &str {
        self.name.trim_end_matches('/').rsplit('/').next().unwrap_or("")
    }
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, Error> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or(Error::Damaged)
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, Error> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or(Error::Damaged)
}

/// An archive held in memory.
pub struct Archive<'a> {
    data: &'a [u8],
    pub entries: Vec<Entry>,
}

impl<'a> Archive<'a> {
    pub fn new(data: &'a [u8]) -> Result<Archive<'a>, Error> {
        // The end of central directory record is in the last 64 KiB + 22 bytes.
        let lowest = data.len().saturating_sub(22 + 0xFFFF);
        let end = (lowest..=data.len().saturating_sub(22))
            .rev()
            .find(|&i| u32_at(data, i) == Ok(0x0605_4b50))
            .ok_or(Error::Damaged)?;
        let count = u16_at(data, end + 10)? as usize;
        let size = u32_at(data, end + 12)?;
        let offset = u32_at(data, end + 16)?;
        if count == 0xFFFF || size == u32::MAX || offset == u32::MAX {
            return Err(Error::Unsupported);
        }
        let mut entries = Vec::with_capacity(count);
        let mut at = offset as usize;
        for _ in 0..count {
            if u32_at(data, at)? != 0x0201_4b50 {
                return Err(Error::Damaged);
            }
            let name_len = u16_at(data, at + 28)? as usize;
            let extra_len = u16_at(data, at + 30)? as usize;
            let comment_len = u16_at(data, at + 32)? as usize;
            let name = data.get(at + 46..at + 46 + name_len).ok_or(Error::Damaged)?;
            entries.push(Entry {
                name: String::from_utf8_lossy(name).into_owned(),
                flags: u16_at(data, at + 8)?,
                method: u16_at(data, at + 10)?,
                crc: u32_at(data, at + 16)?,
                compressed: u32_at(data, at + 20)? as u64,
                size: u32_at(data, at + 24)? as u64,
                local: u32_at(data, at + 42)? as u64,
            });
            at += 46 + name_len + extra_len + comment_len;
        }
        Ok(Archive { data, entries })
    }

    /// The first entry whose name ends with `suffix` (case-insensitive).
    pub fn find(&self, suffix: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| {
            e.name.len() >= suffix.len() && e.name[e.name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        })
    }

    /// The contents of `entry`, at most `limit` bytes.
    pub fn read(&self, entry: &Entry, limit: usize) -> Result<Vec<u8>, Error> {
        if entry.flags & 1 != 0 {
            return Err(Error::Unsupported);
        }
        if entry.size > limit as u64 {
            return Err(Error::TooLarge);
        }
        let at = entry.local as usize;
        if u32_at(self.data, at)? != 0x0403_4b50 {
            return Err(Error::Damaged);
        }
        let start = at + 30 + u16_at(self.data, at + 26)? as usize + u16_at(self.data, at + 28)? as usize;
        let raw = self.data.get(start..start + entry.compressed as usize).ok_or(Error::Damaged)?;
        let out = match entry.method {
            0 => raw.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, limit).map_err(|_| Error::Damaged)?,
            _ => return Err(Error::Unsupported),
        };
        if out.len() as u64 != entry.size {
            return Err(Error::Damaged);
        }
        if crc32(&out) != entry.crc {
            return Err(Error::Checksum);
        }
        Ok(out)
    }
}

/// CRC-32 (IEEE), as ZIP uses it.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    !data.iter().fold(!0u32, |c, &b| TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Builds an archive with one stored and one deflated entry.
    fn sample() -> Vec<u8> {
        let files: [(&str, &[u8], u16); 2] =
            [("freedoom/README.txt", b"hello", 0), ("freedoom/freedoom1.wad", &[7u8; 5000], 8)];
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, body, method) in files {
            let packed = if method == 8 { miniz_oxide::deflate::compress_to_vec(body, 6) } else { body.to_vec() };
            let local = out.len() as u32;
            let header = |sig: u32, central: bool| {
                let mut h = Vec::new();
                h.extend(sig.to_le_bytes());
                if central {
                    h.extend(20u16.to_le_bytes());
                }
                h.extend(20u16.to_le_bytes());
                h.extend(0u16.to_le_bytes());
                h.extend(method.to_le_bytes());
                h.extend([0u8; 4]);
                h.extend(crc32(body).to_le_bytes());
                h.extend((packed.len() as u32).to_le_bytes());
                h.extend((body.len() as u32).to_le_bytes());
                h.extend((name.len() as u16).to_le_bytes());
                h.extend(0u16.to_le_bytes());
                if central {
                    h.extend([0u8; 10]);
                    h.extend(local.to_le_bytes());
                }
                h.extend(name.as_bytes());
                h
            };
            out.extend(header(0x0403_4b50, false));
            out.extend(&packed);
            central.extend(header(0x0201_4b50, true));
        }
        let cd = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend(2u16.to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(cd.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    #[test]
    fn crc_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn reads_stored_and_deflated_entries() {
        let data = sample();
        let zip = Archive::new(&data).unwrap();
        assert_eq!(zip.entries.len(), 2);
        let wad = zip.find("FREEDOOM1.WAD").unwrap();
        assert_eq!(wad.file_name(), "freedoom1.wad");
        assert_eq!(zip.read(wad, 1 << 20).unwrap(), vec![7u8; 5000]);
        assert_eq!(zip.read(&zip.entries[0], 100).unwrap(), b"hello");
        assert_eq!(zip.read(wad, 100), Err(Error::TooLarge));
    }

    #[test]
    fn rejects_damage() {
        let mut data = sample();
        assert!(Archive::new(&data[..data.len() - 30]).is_err());
        assert!(Archive::new(b"not a zip").is_err());
        // Flip a byte of the stored entry: the checksum catches it.
        let at = data.windows(5).position(|w| w == b"hello").unwrap();
        data[at] = b'j';
        let zip = Archive::new(&data).unwrap();
        assert_eq!(zip.read(&zip.entries[0], 100), Err(Error::Checksum));
    }
}
