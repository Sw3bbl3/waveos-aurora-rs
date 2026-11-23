//! Bounded DNS response decoding shared by native clients and host tests.
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Address {
    V4([u8; 4]),
    V6([u8; 16]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Malformed,
    Truncated,
    NotFound,
    Server,
}

fn name_end(m: &[u8], mut i: usize) -> Option<usize> {
    // A name has at most 255 encoded bytes; pointers terminate this occurrence.
    let start = i;
    loop {
        let n = *m.get(i)? as usize;
        if n == 0 {
            return Some(i + 1);
        }
        if n & 0xc0 == 0xc0 {
            let target = ((n & 0x3f) << 8) | *m.get(i + 1)? as usize;
            return (target < m.len()).then_some(i + 2);
        }
        if n > 63 || i + n + 1 > m.len() || i + n + 1 - start > 255 {
            return None;
        }
        i += n + 1;
    }
}

/// Decode an answer for the expected transaction and record type. TTL zero
/// remains zero; callers must not turn it into a positive cache lifetime.
pub fn answer(m: &[u8], id: u16, kind: u16) -> Result<(Vec<Address>, u32), Error> {
    if m.len() < 12 || u16::from_be_bytes([m[0], m[1]]) != id || m[2] & 0xf8 != 0x80 {
        return Err(Error::Malformed);
    }
    if m[2] & 2 != 0 {
        return Err(Error::Truncated);
    }
    match m[3] & 15 {
        0 => (),
        3 => return Err(Error::NotFound),
        _ => return Err(Error::Server),
    }
    let qd = u16::from_be_bytes([m[4], m[5]]);
    let an = u16::from_be_bytes([m[6], m[7]]);
    if qd != 1 {
        return Err(Error::Malformed);
    }
    let mut i = name_end(m, 12).ok_or(Error::Malformed)?;
    let q = m.get(i..i + 4).ok_or(Error::Malformed)?;
    if u16::from_be_bytes([q[0], q[1]]) != kind || q[2..] != [0, 1] {
        return Err(Error::Malformed);
    }
    i += 4;
    let mut out = Vec::new();
    let mut ttl = u32::MAX;
    for _ in 0..an {
        i = name_end(m, i).ok_or(Error::Malformed)?;
        let r = m.get(i..i + 10).ok_or(Error::Malformed)?;
        let ty = u16::from_be_bytes([r[0], r[1]]);
        let class = u16::from_be_bytes([r[2], r[3]]);
        let len = u16::from_be_bytes([r[8], r[9]]) as usize;
        i += 10;
        let data = m.get(i..i + len).ok_or(Error::Malformed)?;
        if class == 1 && (ty == kind || ty == 5) {
            ttl = ttl.min(u32::from_be_bytes([r[4], r[5], r[6], r[7]]));
            match (ty, len) {
                (1, 4) if kind == 1 => out.push(Address::V4(data.try_into().unwrap())),
                (28, 16) if kind == 28 => out.push(Address::V6(data.try_into().unwrap())),
                _ => (),
            }
        }
        i += len;
    }
    Ok((out, if ttl == u32::MAX { 0 } else { ttl }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response() -> Vec<u8> {
        let mut m = alloc::vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0, 1, b'a', 0, 0, 1, 0, 1];
        m.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 5, 0, 4, 1, 2, 3, 4]);
        m
    }
    #[test]
    fn compressed_answer_and_ttl() {
        assert_eq!(answer(&response(), 0x1234, 1), Ok((alloc::vec![Address::V4([1, 2, 3, 4])], 5)));
        let mut m = response();
        m[28] = 0;
        assert_eq!(answer(&m, 0x1234, 1).unwrap().1, 0);
    }
    #[test]
    fn rejects_short_packets_wrong_transactions_and_questions() {
        let m = response();
        for n in 0..m.len() {
            assert!(answer(&m[..n], 0x1234, 1).is_err());
        }
        assert_eq!(answer(&m, 7, 1), Err(Error::Malformed));
        assert_eq!(answer(&m, 0x1234, 28), Err(Error::Malformed));
        let mut bad = m.clone();
        bad[19] = 0xff;
        bad[20] = 0xff;
        assert_eq!(answer(&bad, 0x1234, 1), Err(Error::Malformed));
    }
    #[test]
    fn distinguishes_server_failure_nxdomain_and_tcp_fallback() {
        let mut m = response();
        m[3] = 0x82;
        assert_eq!(answer(&m, 0x1234, 1), Err(Error::Server));
        m[3] = 0x83;
        assert_eq!(answer(&m, 0x1234, 1), Err(Error::NotFound));
        m[2] |= 2;
        assert_eq!(answer(&m, 0x1234, 1), Err(Error::Truncated));
    }
}
