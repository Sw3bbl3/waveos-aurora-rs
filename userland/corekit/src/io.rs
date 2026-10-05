//! Standard streams and `print!`.

use crate::abi::nr;
use core::fmt;

pub struct Stdout;
pub struct Stderr;

pub fn write_fd(fd: u64, bytes: &[u8]) -> crate::Result<usize> {
    let mut done = 0;
    while done < bytes.len() {
        let n = crate::sys::call(nr::WRITE, &[fd, bytes[done..].as_ptr() as u64, (bytes.len() - done) as u64])?;
        if n == 0 {
            break;
        }
        done += n as usize;
    }
    Ok(done)
}

pub fn read_fd(fd: u64, buf: &mut [u8]) -> crate::Result<usize> {
    crate::sys::call(nr::READ, &[fd, buf.as_mut_ptr() as u64, buf.len() as u64]).map(|n| n as usize)
}

impl fmt::Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_fd(1, s.as_bytes()).map(|_| ()).map_err(|_| fmt::Error)
    }
}

impl fmt::Write for Stderr {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_fd(2, s.as_bytes()).map(|_| ()).map_err(|_| fmt::Error)
    }
}

/// Writes to the kernel log (serial console) regardless of stdio redirection.
pub fn log(s: &str) {
    let _ = crate::sys::call(nr::LOG, &crate::sys::str_args(s));
}

/// Formats into one buffer so each `print!` is a single write (keeps lines intact in pipes and logs).
pub fn print_fmt(fd: u64, args: fmt::Arguments) {
    let s = alloc::fmt::format(args);
    let _ = write_fd(fd, s.as_bytes());
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{ $crate::io::print_fmt(1, format_args!($($arg)*)); }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{ $crate::print!("{}\n", format_args!($($arg)*)); }};
}

#[macro_export]
macro_rules! eprintln {
    ($($arg:tt)*) => {{ $crate::io::print_fmt(2, format_args!("{}\n", format_args!($($arg)*))); }};
}
