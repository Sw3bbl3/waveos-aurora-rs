//! AMD Radeon display engines, as far as WaveOS drives them: the DCE
//! blocks of Polaris and Vega and the DCN blocks of every Ryzen APU from
//! Raven to Phoenix and the Navi 1x–3x discrete GPUs.
//!
//! The driver takes over the firmware's (GOP's) mode and never sets one
//! itself: it reads back which timing generators run and which surface
//! they scan, checks that this is exactly the framebuffer the bootloader
//! was handed, and from then on only points those planes at its own
//! buffers in VRAM, moves a cursor and watches hot-plug pins.
//!
//! - [`platform`]: PCI device ids → ASIC → display block and version.
//! - [`regs`] (and the generated `layout`): register offsets per display
//!   block version, from AMD's published register headers, and the fields.
//! - [`takeover`]: reads back what the firmware lit, and decides.
//! - [`scanout`]: surface flips, flip completion, the frame counter, the
//!   cursor.
//! - [`vram`]: where the driver's buffers go in the VRAM aperture.
//! - [`aux`]: the DP AUX engine (DPCD, EDID over I²C-over-AUX), on the
//!   shared [`aurora_igfx::aux::Transport`].
//! - [`hpd`]: hot-plug sense.
//! - [`mock`]: a simulated display engine for tests.
//!
//! None of this has run on real hardware. Register details that could not
//! be confirmed from the public headers are marked `UNCONFIRMED` where
//! they are defined.

#![no_std]

extern crate alloc;

pub mod aux;
pub mod hpd;
mod layout;
pub mod mock;
pub mod platform;
pub mod regs;
pub mod scanout;
pub mod takeover;
pub mod vram;

pub use aurora_igfx::Mmio;
