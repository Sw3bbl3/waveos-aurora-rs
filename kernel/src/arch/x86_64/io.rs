//! Port I/O (`in`/`out`). `Port::<T>::new(port)` with `unsafe` `read` and
//! `write`; T is u8, u16 or u32.

pub use x86_64::instructions::port::Port;
