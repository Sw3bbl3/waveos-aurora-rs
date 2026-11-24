//! The kernel's part in system updates (see `corekit::update` for the
//! scheme and `bootloader/src/slots.rs` for the other half).
//!
//! - Which slot this system started from (`BootInfo` flags), answered to
//!   programs as the `system_slot` preference.
//! - Marking a start on trial good: once the desktop has drawn its first
//!   frame, `system_tries` is removed from boot.conf, so the bootloader
//!   keeps this system; and the bootloader the update shipped is put in
//!   place (only now, after the new system has proved it starts).
//! - "Update and Restart" in the power dialog: when an update is installed
//!   and ready (`/.system/ready`), boot.conf is pointed at it first.
use crate::sync::IrqMutex;
use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const SLOTS: &str = "/.system";
const BOOT_CONF: &str = "/Boot/aurora/boot.conf";
/// The bootloader on the EFI partition and its name in a slot (this processor's).
const BOOT_FILE: &str = if cfg!(target_arch = "aarch64") { "BOOTAA64.EFI" } else { "BOOTX64.EFI" };
const BOOTLOADER: &str = if cfg!(target_arch = "aarch64") { "/Boot/EFI/BOOT/BOOTAA64.EFI" } else { "/Boot/EFI/BOOT/BOOTX64.EFI" };

static FLAGS: AtomicU32 = AtomicU32::new(0);
static MARKED: AtomicBool = AtomicBool::new(false);
/// What happened at this start, for the log and the self-test.
static OUTCOME: IrqMutex<Option<String>> = IrqMutex::new(None);

/// Records the bootloader's flags (at boot).
pub fn init(flags: u32) {
    FLAGS.store(flags, Ordering::Relaxed);
}

/// The slot this system started from: `esp`, `a` or `b`.
pub fn booted_slot() -> &'static str {
    let f = FLAGS.load(Ordering::Relaxed);
    if f & bootinfo::BOOT_SLOT_A != 0 {
        "a"
    } else if f & bootinfo::BOOT_SLOT_B != 0 {
        "b"
    } else {
        "esp"
    }
}

/// Whether the bootloader can start slots at all.
pub fn bootloader_knows_slots() -> bool {
    FLAGS.load(Ordering::Relaxed) & bootinfo::BOOT_SLOTS != 0
}

fn read_conf() -> String {
    crate::fs::read_all(BOOT_CONF).ok().and_then(|b| String::from_utf8(b).ok()).unwrap_or_default()
}

fn option(conf: &str, key: &str) -> Option<String> {
    conf.lines().find_map(|l| {
        let (k, v) = l.trim().split_once('=')?;
        (k.trim() == key).then(|| String::from(v.trim()))
    })
}

/// boot.conf with `set` applied (None removes a key).
fn edited(conf: &str, set: &[(&str, Option<&str>)]) -> String {
    let mut out = String::new();
    for line in conf.lines() {
        let key = line.trim().split_once('=').map(|(k, _)| k.trim());
        if key.is_none_or(|k| !set.iter().any(|(s, _)| *s == k)) {
            out.push_str(line);
            out.push('\n');
        }
    }
    for (k, v) in set {
        if let Some(v) = v {
            out.push_str(&format!("{k}={v}\n"));
        }
    }
    out
}

/// Writes boot.conf: a new file, then renamed over the old one.
fn write_conf(text: &str) -> Result<(), isize> {
    let tmp = "/Boot/aurora/boot.new";
    crate::fs::write_all(tmp, text.as_bytes())?;
    if crate::fs::rename(tmp, BOOT_CONF).is_err() {
        let _ = crate::fs::unlink(BOOT_CONF);
        crate::fs::rename(tmp, BOOT_CONF)?;
    }
    crate::fs::sync_all();
    Ok(())
}

/// The desktop is up: this start was good (see the module docs). Runs once.
pub fn mark_good() {
    if MARKED.swap(true, Ordering::Relaxed) {
        return;
    }
    start_checks();
    crate::sched::spawn("sysupdate", || {
        crate::fs::perm::system(|| {
            let conf = read_conf();
            let slot = booted_slot();
            let configured = option(&conf, "system").unwrap_or_else(|| String::from("esp"));
            let outcome = if option(&conf, "system_tries").is_none() {
                if let Some(f) = option(&conf, "system_failed") {
                    format!("started {slot}; system {f} failed to start and was abandoned")
                } else {
                    format!("started {slot}")
                }
            } else if configured != slot {
                // An older bootloader that doesn't know slots started its own.
                let _ = write_conf(&edited(
                    &conf,
                    &[("system", Some(slot)), ("system_tries", None), ("system_failed", Some(&configured))],
                ));
                format!("started {slot}, not {configured}: the bootloader can't start updates")
            } else {
                match write_conf(&edited(&conf, &[("system_tries", None)])) {
                    Ok(()) => {
                        install_bootloader(slot);
                        format!("started {slot}; marked good")
                    }
                    Err(e) => format!("started {slot}; could not mark it good ({})", aurora_abi::err::name(e)),
                }
            };
            log!("update", "{outcome}");
            *OUTCOME.lock() = Some(outcome);
        });
    });
}

/// After a good start of an update, its bootloader replaces the old one
/// (new file, then rename) if they differ.
fn install_bootloader(slot: &str) {
    if slot == "esp" {
        return;
    }
    let Ok(new) = crate::fs::read_all(&format!("{SLOTS}/{slot}/{BOOT_FILE}")) else { return };
    if new.len() < 4096 || &new[..2] != b"MZ" || crate::fs::read_all(BOOTLOADER).is_ok_and(|old| old == new) {
        return;
    }
    let tmp = if cfg!(target_arch = "aarch64") { "/Boot/EFI/BOOT/BOOTAA64.NEW" } else { "/Boot/EFI/BOOT/BOOTX64.NEW" };
    let ok = crate::fs::write_all(tmp, &new).is_ok()
        && crate::fs::read_all(tmp).is_ok_and(|b| b == new)
        && (crate::fs::rename(tmp, BOOTLOADER).is_ok()
            || (crate::fs::unlink(BOOTLOADER).is_ok() && crate::fs::rename(tmp, BOOTLOADER).is_ok()));
    crate::fs::sync_all();
    log!("update", "{} the bootloader of system {slot}", if ok { "installed" } else { "could not install" });
}

/// What happened at this start (once the desktop is up).
#[allow(dead_code)] // the self-test reads it
pub fn outcome() -> Option<String> {
    OUTCOME.lock().clone()
}

/// An installed update ready for a restart: (slot, version).
pub fn ready() -> Option<(String, String)> {
    let text = crate::fs::read_all(&format!("{SLOTS}/ready")).ok()?;
    let text = String::from_utf8(text).ok()?;
    let (slot, version) = text.trim().split_once('\t')?;
    let ok = matches!(slot, "a" | "b") && crate::fs::stat(&format!("{SLOTS}/{slot}/update.txt")).is_ok();
    ok.then(|| (String::from(slot), String::from(version)))
}

/// Points the next start at the ready update ("Update and Restart").
pub fn arm() -> Result<String, isize> {
    let (slot, version) = ready().ok_or(aurora_abi::err::ENOENT)?;
    crate::fs::perm::system(|| {
        let conf = read_conf();
        let previous = booted_slot();
        if !bootloader_knows_slots() {
            // This bootloader can't start updates: the update's own first.
            install_bootloader(&slot);
        }
        write_conf(&edited(
            &conf,
            &[
                ("system", Some(&slot)),
                ("system_tries", Some("3")),
                ("system_previous", Some(previous)),
                ("system_failed", None),
            ],
        ))?;
        let _ = crate::fs::unlink(&format!("{SLOTS}/ready"));
        crate::fs::sync_all();
        log!("update", "the next start uses system {slot} ({version})");
        Ok(version)
    })
}

/// The first background check for updates, after the desktop is up.
const FIRST_CHECK_MS: u64 = 3 * 60 * 1000;
/// Then this often (the Store itself checks at most once a day).
const CHECK_EVERY_MS: u64 = 6 * 60 * 60 * 1000;

/// Sleeps `ms` in full: a sleeping task is also woken early (when a program
/// it started ends, for one), and that must not start another check.
fn rest(ms: u64) {
    let until = crate::time::uptime_ms() + ms;
    loop {
        let now = crate::time::uptime_ms();
        if now >= until {
            return;
        }
        crate::sched::sleep_ms(until - now);
    }
}

/// Starts the background checks for app and system updates: the Store,
/// run as `Store.elf --check` for the logged-in account, looks at the
/// configured repositories and posts a notification when there is
/// something new. Off with the `software_update_check` preference (and in
/// the self-test build, which runs its own checks).
fn start_checks() {
    if cfg!(feature = "ktest") {
        return;
    }
    crate::sched::spawn("update-check", || {
        rest(FIRST_CHECK_MS);
        loop {
            if super::prefs::get("software_update_check").as_deref() != Some("0") {
                let path = "/System/Apps/Store.elf";
                if let Err(e) = crate::proc::spawn(path, &[path, "--check"], [None, None, None], 0) {
                    log!("update", "could not start the update check: {}", aurora_abi::err::name(e));
                }
            }
            rest(CHECK_EVERY_MS);
        }
    });
}
