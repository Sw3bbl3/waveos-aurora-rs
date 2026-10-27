//! Wi-Fi for WaveOS: 802.11 frames, network discovery, and WPA2-Personal —
//! the supplicant (4-way and group key handshakes) and CCMP encryption.
//!
//! Hardware-independent and `no_std`, so it is shared by the kernel's
//! Wi-Fi drivers and tested on the host against the standard's test
//! vectors and an independent implementation (see `tests/`).

#![no_std]

extern crate alloc;

pub mod crypto;
pub mod frame;
pub mod iwlfw;
pub mod rsn;
pub mod sim;
pub mod station;
pub mod supplicant;

pub use frame::{Bss, Mac};
pub use rsn::Security;
