//! Voyage — WaveOS's web browser, on WebKit (the engine of Safari). Starts
//! the browser, a Linux program built for the Linux packages on the disk,
//! through `/System/Linux/bin/voyage` (which starts the Wayland bridge
//! first), with the addresses or files it was given, in the dark theme when
//! WaveOS is dark. Without WebKit on the disk (an older disk with no room
//! for it), Firefox opens instead, or else NetSurf.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use corekit::{abi::pref, fs, notify, prefs, process};

/// Voyage itself, in the disk's Linux tree.
const VOYAGE: &str = "/Linux/usr/lib/voyage/voyage";
const SHELL: &str = "/System/Linux/bin/busybox";
const LAUNCHER: &str = "/System/Linux/bin/voyage";
/// The browsers that stand in for it.
const FIREFOX: &str = "/Linux/usr/lib/firefox-esr/firefox-esr";
const FIREFOX_APP: &str = "/System/Apps/Firefox.elf";
const NETSURF: &str = "/System/NetSurf/netsurf";

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let pages: Vec<&str> = args.iter().skip(1).map(|a| a.as_str()).collect();
    if !fs::exists(VOYAGE) {
        if fs::exists(FIREFOX) && process::spawn(FIREFOX_APP, &pages, process::Stdio::default()).is_ok() {
            notify::post(
                "Voyage isn't on this disk yet",
                "Firefox opened in its place. Starting WaveOS with cargo xtask run puts Voyage on the disk.",
            );
            return 0;
        }
        // NetSurf takes one page.
        let page = pages.first().copied().into_iter().collect::<Vec<_>>();
        if process::spawn(NETSURF, &page, process::Stdio::default()).is_ok() {
            return 0;
        }
        notify::post(
            "Voyage isn't on this disk",
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
            notify::post("Voyage didn't start", "The Linux shell couldn't be started.");
            1
        }
    }
}
