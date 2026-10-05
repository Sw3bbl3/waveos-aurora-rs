//! Notifications.

use crate::abi::nr;
use crate::sys::call;

/// Shows a notification banner (kept in the Notification Center).
pub fn post(title: &str, body: &str) {
    let _ = call(nr::NOTIFY, &[title.as_ptr() as u64, title.len() as u64, body.as_ptr() as u64, body.len() as u64]);
}
