//! ACPI: the static tables parsed early in boot ([`tables`]), and the
//! runtime ([`runtime`]) — the AML interpreter from the `acpi` crate on its
//! own task, which handles the power button, batteries, the power adapter,
//! the lid and sleep states; and [`devices`], the serial-bus devices
//! (I²C touchpads and touch screens) only ACPI describes.

pub mod devices;
mod events;
mod handler;
pub mod processor;
mod runtime;
mod tables;

pub use runtime::{is_acpi_task, on_panic, power_info, sleep_type, start};
// For sleep (S3), x86 only.
#[cfg(target_arch = "x86_64")]
pub use runtime::{finish_sleep, prepare_sleep};
#[cfg(feature = "ktest")]
pub use runtime::{running, test_panic};
pub use tables::*;
