//! On-device installation regression test; touches only its unique test app.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::{
    format,
    string::{String, ToString},
};
use corekit::{apps, fs, println};
corekit::entry!(main);
fn main(_: corekit::Args) -> i32 {
    match run() {
        Ok(()) => {
            println!("GINA CHECK: all 8 lifecycle checks passed");
            0
        }
        Err(e) => {
            println!("GINA CHECK FAILED: {e}");
            1
        }
    }
}
fn run() -> Result<(), String> {
    let io = |e: corekit::Error| format!("{e}");
    let fixture = fs::read("/System/Developer/Examples/AuroraKitGallery.gina").map_err(io)?;
    let parsed = gina::Package::parse(&fixture).map_err(ToString::to_string)?;
    let id = format!("dev.constellation.check-p{}-t{}", corekit::process::pid(), corekit::time::uptime_ms());
    let mut manifest = parsed.manifest.clone();
    manifest.id = id.clone();
    manifest.name = "GINA installation test".into();
    let files: alloc::vec::Vec<_> =
        parsed.files.iter().filter(|f| f.path != "manifest.toml").map(|f| (f.path.as_str(), f.bytes)).collect();
    let first = gina::pack(&manifest, &files).map_err(ToString::to_string)?;
    let a = apps::install(&first, |_, _| {})?;
    if a.manifest.id != id {
        return Err("wrong identity".into());
    }
    println!("PASS initial install");
    let probe = format!("/AppData/{id}/probe.txt");
    fs::write(&probe, b"keep my data").map_err(io)?;
    let again = apps::install(&first, |_, _| {})?;
    if again.directory != a.directory {
        return Err("reinstall changed generation".into());
    }
    println!("PASS identical reinstall");
    let mut malformed = first.clone();
    malformed[148] ^= 1;
    if apps::install(&malformed, |_, _| {}).is_ok() {
        return Err("malformed package accepted".into());
    }
    if apps::installed(&id)?.directory != a.directory {
        return Err("failed update changed active app".into());
    }
    println!("PASS malformed update preserves active app");
    manifest.version = "2.0.0".into();
    let update = gina::pack(&manifest, &files).map_err(ToString::to_string)?;
    let b = apps::install(&update, |_, _| {})?;
    if b.directory == a.directory || b.manifest.version != "2.0.0" {
        return Err("update not selected".into());
    }
    println!("PASS versioned update");
    if fs::read(&probe).map_err(io)? != b"keep my data" {
        return Err("update lost app data".into());
    }
    println!("PASS update preserves app data");
    apps::uninstall(&id, false)?;
    if apps::installed(&id).is_ok() || fs::read(&probe).map_err(io)? != b"keep my data" {
        return Err("uninstall retention failed".into());
    }
    println!("PASS uninstall retains data");
    apps::install(&update, |_, _| {})?;
    if fs::read(&probe).map_err(io)? != b"keep my data" {
        return Err("reinstall lost data".into());
    }
    println!("PASS reinstall retains data");
    apps::uninstall(&id, true)?;
    if fs::exists(&format!("/AppData/{id}")) || fs::exists(&format!("/Applications/{id}")) {
        return Err("explicit removal incomplete".into());
    }
    println!("PASS explicit test-data deletion");
    Ok(())
}
