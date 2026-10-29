//! Firefox — WaveOS's web browser. Starts Debian's Firefox ESR, a Linux
//! program, through `/System/Linux/bin/firefox` (which starts the Wayland
//! bridge first), with the addresses or files it was given, in the dark
//! theme when WaveOS is dark. Firefox comes with the Linux packages on the
//! disk (`cargo xtask run` puts them there); without them, NetSurf opens
//! instead.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use corekit::{abi::pref, fs, notify, prefs, process};

/// Firefox itself, in the disk's Linux tree.
const FIREFOX: &str = "/Linux/usr/lib/firefox-esr/firefox-esr";
const SHELL: &str = "/System/Linux/bin/busybox";
const LAUNCHER: &str = "/System/Linux/bin/firefox";
/// The small browser in the system image.
const NETSURF: &str = "/System/NetSurf/netsurf";

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let pages: Vec<&str> = args.iter().skip(1).map(|a| a.as_str()).collect();
    if !fs::exists(FIREFOX) {
        // NetSurf takes one page.
        let page = pages.first().copied().into_iter().collect::<Vec<_>>();
        if process::spawn(NETSURF, &page, process::Stdio::default()).is_ok() {
            return 0;
        }
        notify::post(
            "Firefox isn't on this disk",
            "It comes with the Linux packages: cargo xtask run --fresh-disk puts them on the disk.",
        );
        return 1;
    }
    let mut argv = Vec::from(["sh", LAUNCHER]);
    if prefs::get_bool(pref::DARK) {
        argv.push("--waveos-dark");
    }
    argv.extend(pages);
    match process::spawn(SHELL, &argv, process::Stdio::default()) {
        Ok(_) => 0,
        Err(_) => {
            notify::post("Firefox didn't start", "The Linux shell couldn't be started.");
            1
        }
    }
}
