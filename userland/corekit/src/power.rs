//! Battery, power adapter and lid (from ACPI).

use crate::abi::{nr, PowerInfo};
use crate::sys::call;

pub use crate::abi::power::*;

pub fn info() -> PowerInfo {
    let mut p = PowerInfo::default();
    let _ = call(nr::POWER_INFO, &[&mut p as *mut PowerInfo as u64]);
    p
}
