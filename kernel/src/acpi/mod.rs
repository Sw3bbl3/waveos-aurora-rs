//! ACPI: the static tables parsed early in boot ([`tables`]), and the
//! runtime ([`runtime`]) — the AML interpreter from the `acpi` crate on its
//! own task, which handles the power button, batteries, the power adapter,
//! the lid and sleep states.

mod events;
mod handler;
mod runtime;
mod tables;

pub use runtime::{finish_sleep, is_acpi_task, on_panic, power_info, prepare_sleep, sleep_type, start};
#[cfg(feature = "ktest")]
pub use runtime::{running, test_panic};
pub use tables::*;
