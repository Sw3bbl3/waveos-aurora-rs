//! CRC-32 (IEEE 802.3) and CRC-32C (Castagnoli), table-driven.

const fn table(poly: u32) -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { poly ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const TABLE: [u32; 256] = table(0xEDB8_8320);
const TABLE_C: [u32; 256] = table(0x82F6_3B78);

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finish()
}

/// CRC-32 over data given in pieces.
pub(crate) struct Crc32(u32);

impl Crc32 {
    pub(crate) fn new() -> Crc32 {
        Crc32(!0)
    }
    pub(crate) fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.0 = TABLE[((self.0 ^ b as u32) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }
    pub(crate) fn finish(&self) -> u32 {
        !self.0
    }
}

/// CRC-32C over data given in pieces. Unlike CRC-32 it still tells blocks
/// apart that embed their own CRC-32 (superblocks): see `write_journal`.
pub(crate) struct Crc32c(u32);

impl Crc32c {
    pub(crate) fn new() -> Crc32c {
        Crc32c(!0)
    }
    pub(crate) fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.0 = TABLE_C[((self.0 ^ b as u32) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }
    pub(crate) fn finish(&self) -> u32 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let mut c = Crc32c::new();
        c.update(b"123456789");
        assert_eq!(c.finish(), 0xE306_9283);
    }

    /// Why the journal needs a second checksum: CRC-32 over anything
    /// followed by its own CRC-32 comes out the same.
    #[test]
    fn crc32_cannot_tell_self_checked_blocks_apart() {
        let block = |fill: u8| {
            let mut b = [fill; 64].to_vec();
            let c = crc32(&b);
            b.extend_from_slice(&c.to_le_bytes());
            b
        };
        let (a, b) = (block(1), block(2));
        assert_ne!(a, b);
        assert_eq!(crc32(&a), crc32(&b));
        let (mut ca, mut cb) = (Crc32c::new(), Crc32c::new());
        ca.update(&a);
        cb.update(&b);
        assert_ne!(ca.finish(), cb.finish());
    }
}
