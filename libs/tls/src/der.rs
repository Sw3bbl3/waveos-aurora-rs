//! A minimal DER reader: enough ASN.1 for X.509 certificates.

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const OID: u8 = 0x06;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// A position in DER-encoded bytes.
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
}

/// One element: its tag, its contents, and its whole encoding.
#[derive(Clone, Copy, Debug)]
pub struct Element<'a> {
    pub tag: u8,
    pub value: &'a [u8],
    pub raw: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.data.first().copied()
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Element<'a>> {
        let d = self.data;
        let tag = *d.first()?;
        let first = *d.get(1)?;
        let (len, header) = if first < 0x80 {
            (first as usize, 2)
        } else {
            let n = (first & 0x7F) as usize;
            if n == 0 || n > 4 {
                return None;
            }
            let mut len = 0usize;
            for k in 0..n {
                len = len << 8 | *d.get(2 + k)? as usize;
            }
            (len, 2 + n)
        };
        let end = header.checked_add(len)?;
        if end > d.len() {
            return None;
        }
        self.data = &d[end..];
        Some(Element { tag, value: &d[header..end], raw: &d[..end] })
    }

    /// The next element, which must have `tag`.
    pub fn expect(&mut self, tag: u8) -> Option<Element<'a>> {
        let e = self.next()?;
        (e.tag == tag).then_some(e)
    }

    /// Skips an optional element with `tag`; returns it if present.
    pub fn optional(&mut self, tag: u8) -> Option<Element<'a>> {
        if self.peek_tag() == Some(tag) {
            self.next()
        } else {
            None
        }
    }
}

impl<'a> Element<'a> {
    pub fn reader(&self) -> Reader<'a> {
        Reader::new(self.value)
    }
}

/// An unsigned big-endian INTEGER's bytes without the leading sign zero.
pub fn unsigned<'a>(e: &Element<'a>) -> &'a [u8] {
    let v = e.value;
    if v.len() > 1 && v[0] == 0 {
        &v[1..]
    } else {
        v
    }
}

/// UTCTime or GeneralizedTime as seconds since 1970-01-01 (UTC).
pub fn time(e: &Element) -> Option<i64> {
    let s = core::str::from_utf8(e.value).ok()?;
    let s = s.strip_suffix('Z')?;
    let (year, rest) = match e.tag {
        UTC_TIME => {
            let y: i64 = s.get(0..2)?.parse().ok()?;
            (if y < 50 { 2000 + y } else { 1900 + y }, s.get(2..)?)
        }
        GENERALIZED_TIME => (s.get(0..4)?.parse().ok()?, s.get(4..)?),
        _ => return None,
    };
    let num = |r: core::ops::Range<usize>| -> Option<i64> { rest.get(r)?.parse().ok() };
    let (month, day, hour, min) = (num(0..2)?, num(2..4)?, num(4..6)?, num(6..8)?);
    let sec = num(8..10).unwrap_or(0);
    Some(days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested() {
        // SEQUENCE { INTEGER 5, OCTET STRING "hi" }
        let d = [0x30, 0x07, 0x02, 0x01, 0x05, 0x04, 0x02, b'h', b'i'];
        let mut r = Reader::new(&d);
        let seq = r.expect(SEQUENCE).unwrap();
        assert!(r.is_empty());
        let mut inner = seq.reader();
        assert_eq!(inner.expect(INTEGER).unwrap().value, &[5]);
        assert_eq!(inner.expect(OCTET_STRING).unwrap().value, b"hi");
        assert!(inner.next().is_none());
    }

    #[test]
    fn long_lengths_and_times() {
        let mut d = alloc::vec![0x04, 0x81, 200];
        d.extend(core::iter::repeat_n(7u8, 200));
        assert_eq!(Reader::new(&d).next().unwrap().value.len(), 200);
        let t = [UTC_TIME, 13, b'2', b'4', b'0', b'1', b'0', b'2', b'0', b'3', b'0', b'4', b'0', b'5', b'Z'];
        let e = Reader::new(&t).next().unwrap();
        assert_eq!(time(&e), Some(1_704_164_645)); // 2024-01-02 03:04:05 UTC
    }
}
