//! chmod MODE FILE... — permission bits: octal (`640`) or symbolic
//! (`u+x`, `go-w`, `a=r`, comma-separated).
#![no_std]
#![no_main]
extern crate alloc;
use corekit::users;
corekit::entry!(main);

/// Applies a mode argument to `mode` (None if it doesn't parse).
fn apply(arg: &str, mode: u32, dir: bool) -> Option<u32> {
    if !arg.is_empty() && arg.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return u32::from_str_radix(arg, 8).ok().filter(|m| *m <= 0o7777);
    }
    let mut m = mode;
    for clause in arg.split(',') {
        let op_at = clause.find(['+', '-', '='])?;
        let (who, rest) = clause.split_at(op_at);
        let op = rest.as_bytes()[0];
        let mut mask = 0;
        for c in if who.is_empty() { "a" } else { who }.chars() {
            mask |= match c {
                'u' => 0o4700,
                'g' => 0o2070,
                'o' => 0o1007,
                'a' => 0o7777,
                _ => return None,
            };
        }
        let mut bits = 0;
        for c in rest[1..].chars() {
            bits |= match c {
                'r' => 0o444,
                'w' => 0o222,
                'x' => 0o111,
                // Execute only for folders or what is already executable.
                'X' if dir || mode & 0o111 != 0 => 0o111,
                'X' => 0,
                's' => 0o6000,
                't' => 0o1000,
                _ => return None,
            };
        }
        let bits = bits & mask;
        m = match op {
            b'+' => m | bits,
            b'-' => m & !bits,
            _ => (m & !(mask & 0o777)) | bits,
        };
    }
    Some(m)
}

fn main(args: corekit::Args) -> i32 {
    if args.len() < 3 {
        corekit::eprintln!("usage: chmod MODE FILE...");
        return 2;
    }
    let mut status = 0;
    for f in &args[2..] {
        let p = coreutils::path(f);
        let result = users::owner(&p).and_then(|o| {
            let dir = corekit::fs::is_dir(&p);
            match apply(&args[1], o.mode, dir) {
                Some(m) => users::chmod(&p, m),
                None => Err(corekit::Error(-corekit::abi::err::EINVAL)),
            }
        });
        if let Err(e) = result {
            corekit::eprintln!("chmod: {f}: {e}");
            status = 1;
        }
    }
    status
}
