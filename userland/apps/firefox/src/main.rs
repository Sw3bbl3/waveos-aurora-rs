//! Firefox ESR — starts Debian's Firefox ESR, a Linux program, through
//! `/System/Linux/bin/firefox` (which starts the Wayland bridge first).
//! Firefox comes with the Linux packages on the disk (`cargo xtask run
//! --linux-root`); without them, a notification says so.

#![no_std]
#![no_main]

use corekit::{fs, notify, process};

/// Firefox itself, in the disk's Linux tree.
const FIREFOX: &str = "/Linux/usr/lib/firefox-esr/firefox-esr";
const SHELL: &str = "/System/Linux/bin/busybox";
const LAUNCHER: &str = "/System/Linux/bin/firefox";

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    if !fs::exists(FIREFOX) {
        notify::post(
            "Firefox ESR isn't installed",
            "It comes with the Linux packages: build the disk with cargo xtask run --linux-root.",
        );
        return 1;
    }
    match process::spawn(SHELL, &["sh", LAUNCHER], process::Stdio::default()) {
        Ok(_) => 0,
        Err(_) => {
            notify::post("Firefox ESR didn't start", "The Linux shell couldn't be started.");
            1
        }
    }
}
