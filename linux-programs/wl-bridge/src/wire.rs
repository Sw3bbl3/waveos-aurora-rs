//! The Wayland wire format: each message is the object id, then the
//! message size (high 16 bits) and opcode (low 16), then the arguments as
//! 32-bit words. Strings and arrays carry their length and are padded to
//! 4 bytes; file descriptors travel beside the bytes (`SCM_RIGHTS`).

use crate::sys::Fd;
use std::collections::VecDeque;

#[derive(Debug)]
#[allow(dead_code)] // arrays are parsed but no request needs one
pub enum Arg {
    Int(i32),
    Uint(u32),
    Fixed(i32),
    Str(Option<String>),
    Obj(u32),
    New(u32),
    Array(Vec<u8>),
    Fd(Fd),
}

impl Arg {
    pub fn int(&self) -> i32 {
        match *self {
            Arg::Int(v) | Arg::Fixed(v) => v,
            Arg::Uint(v) | Arg::Obj(v) | Arg::New(v) => v as i32,
            _ => 0,
        }
    }
    pub fn uint(&self) -> u32 {
        self.int() as u32
    }
    /// A fixed-point number as a float.
    pub fn fixed(&self) -> f64 {
        self.int() as f64 / 256.0
    }
    pub fn str(&self) -> &str {
        match self {
            Arg::Str(Some(s)) => s,
            _ => "",
        }
    }
    pub fn fd(&self) -> Fd {
        match *self {
            Arg::Fd(f) => f,
            _ => -1,
        }
    }
}

pub struct Message {
    pub object: u32,
    pub opcode: u16,
    pub args: Vec<Arg>,
}

/// The first whole message in `buf`, with how many bytes it took; `None`
/// until all of it has arrived. `sig` gives the argument signature of a
/// request (object, opcode); an unknown one is an error.
pub fn parse(
    buf: &[u8],
    fds: &mut VecDeque<Fd>,
    sig: impl Fn(u32, u16) -> Option<&'static str>,
) -> Result<Option<(Message, usize)>, String> {
    if buf.len() < 8 {
        return Ok(None);
    }
    let word = |at: usize| u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
    let object = word(0);
    let opcode = (word(4) & 0xffff) as u16;
    let size = (word(4) >> 16) as usize;
    if size < 8 || !size.is_multiple_of(4) {
        return Err(format!("bad message size {size}"));
    }
    if buf.len() < size {
        return Ok(None);
    }
    let Some(sig) = sig(object, opcode) else {
        return Err(format!("unknown request {opcode} on object {object}"));
    };
    let mut at = 8;
    let mut args = Vec::new();
    let mut nullable = false;
    for c in sig.chars() {
        if c == '?' {
            nullable = true;
            continue;
        }
        if c == 'h' {
            args.push(Arg::Fd(fds.pop_front().ok_or("missing file descriptor")?));
            continue;
        }
        if at + 4 > size {
            return Err(String::from("message too short"));
        }
        let w = word(at);
        at += 4;
        args.push(match c {
            'i' => Arg::Int(w as i32),
            'u' => Arg::Uint(w),
            'f' => Arg::Fixed(w as i32),
            'o' => Arg::Obj(w),
            'n' => Arg::New(w),
            's' | 'a' => {
                let len = w as usize;
                let padded = (len + 3) & !3;
                if at + padded > size {
                    return Err(String::from("string past the message"));
                }
                let bytes = &buf[at..at + len];
                at += padded;
                if c == 'a' {
                    Arg::Array(bytes.to_vec())
                } else if len == 0 && nullable {
                    Arg::Str(None)
                } else {
                    let text = bytes.strip_suffix(&[0]).unwrap_or(bytes);
                    Arg::Str(Some(String::from_utf8_lossy(text).into_owned()))
                }
            }
            _ => return Err(format!("bad signature {sig}")),
        });
        nullable = false;
    }
    Ok(Some((Message { object, opcode, args }, size)))
}

/// An event argument.
pub enum Out<'a> {
    I(i32),
    U(u32),
    F(f64),
    S(&'a str),
    O(u32),
    N(u32),
    A(&'a [u8]),
    H(Fd),
}

/// Appends an event to `buf` (and its descriptors to `fds`).
pub fn encode(buf: &mut Vec<u8>, fds: &mut Vec<Fd>, object: u32, opcode: u16, args: &[Out]) {
    let start = buf.len();
    buf.extend_from_slice(&object.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    let bytes = |b: &[u8], buf: &mut Vec<u8>| {
        buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
        buf.extend_from_slice(b);
        buf.resize(buf.len() + ((4 - b.len() % 4) % 4), 0);
    };
    for a in args {
        match *a {
            Out::I(v) => buf.extend_from_slice(&v.to_le_bytes()),
            Out::U(v) | Out::O(v) | Out::N(v) => buf.extend_from_slice(&v.to_le_bytes()),
            Out::F(v) => buf.extend_from_slice(&((v * 256.0) as i32).to_le_bytes()),
            Out::S(s) => {
                let mut z = s.as_bytes().to_vec();
                z.push(0);
                bytes(&z, buf);
            }
            Out::A(a) => bytes(a, buf),
            Out::H(fd) => fds.push(fd),
        }
    }
    let size = (buf.len() - start) as u32;
    buf[start + 4..start + 8].copy_from_slice(&((size << 16) | opcode as u32).to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let (mut buf, mut fds) = (Vec::new(), Vec::new());
        encode(&mut buf, &mut fds, 3, 0, &[Out::U(7), Out::S("wl_shm"), Out::U(1), Out::N(9), Out::H(5)]);
        let mut queue: VecDeque<Fd> = fds.into_iter().collect();
        let (m, n) = parse(&buf, &mut queue, |_, _| Some("usunh")).unwrap().unwrap();
        assert_eq!(n, buf.len());
        assert_eq!((m.object, m.opcode), (3, 0));
        assert_eq!(m.args[1].str(), "wl_shm");
        assert_eq!(m.args[3].uint(), 9);
        assert_eq!(m.args[4].fd(), 5);
        assert!(parse(&buf[..buf.len() - 1], &mut queue, |_, _| Some("usun")).unwrap().is_none());
    }
}
