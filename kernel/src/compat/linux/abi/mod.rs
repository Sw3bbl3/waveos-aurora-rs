//! What differs between the Linux ABIs the layer speaks: x86-64 and arm64
//! (and, on a branch, 32-bit i386 on x86-64).
//!
//! Every call is implemented once, for every ABI, in the shared modules
//! (`fs`, `net`, `mm`, `process`, `signal`…), under its x86-64 number:
//! [`nr`] is the layer's own numbering. An ABI module says how its programs'
//! calls reach those implementations and supplies what is laid out
//! differently in memory:
//!
//! - **Calls:** [`native::lookup`] turns the number a program used into a
//!   [`Call`]: the shared call with that meaning, the ABI's own handler, or
//!   none. x86-64 is the identity; arm64's generic table and the data it
//!   is made of (`aurora_linuxabi::aarch64::TABLE`) are checked against
//!   Linux's headers on the host.
//! - **Memory:** `struct stat` ([`native::put_stat`]), `struct
//!   epoll_event` ([`native::EPOLL_EVENT_SIZE`], [`native::EPOLL_DATA_AT`]),
//!   the open flags that differ ([`native::O_DIRECTORY`],
//!   [`native::O_NOFOLLOW`]), the signal stack minimum.
//! - **Machine state:** the signal frame, `rt_sigreturn` and restarting an
//!   interrupted call ([`native::signal`]), `uname`'s machine and the
//!   auxiliary vector's CPU entries ([`native::MACHINE`],
//!   [`native::auxv`]).
//!
//! The shared code uses `abi::native::…` and never `cfg(target_arch)`.

pub mod x86_64;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;

pub use x86_64::nr;

#[cfg(target_arch = "aarch64")]
pub use aarch64 as native;
#[cfg(target_arch = "x86_64")]
pub use x86_64 as native;

/// What a program's call number means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(target_arch = "x86_64", allow(dead_code))] // (x86-64 numbers are all shared)
pub enum Call {
    /// The shared call with this ([`nr`]) number.
    Shared(u64),
    /// The ABI's own handler (`native::own`).
    Own,
    /// Not a call this ABI has, or one the layer doesn't provide.
    None,
}
