//! Installed GINA apps, as the desktop sees them: those installed for
//! everyone (`/Applications`) and for the logged-in account
//! (`~/Library/Applications`). Each app's live version is named by its
//! atomic `active` pointer (see `corekit::apps`); the manifest there gives
//! its name, icon, categories and file associations.
use crate::gui::icons::{self, Icon};
use alloc::{format, string::String, vec::Vec};

#[derive(Clone)]
pub struct App {
    pub manifest: gina::Manifest,
    /// The program to run.
    pub path: String,
    /// The live generation's folder.
    pub dir: String,
    pub icon: Icon,
}

/// Where apps are installed: the logged-in account's own first.
pub fn roots() -> Vec<String> {
    let home = crate::users::db::get(crate::users::session().uid).map(|u| u.home).unwrap_or_else(|| String::from("/"));
    let mine = if home.ends_with('/') {
        format!("{home}Library/Applications")
    } else {
        format!("{home}/Library/Applications")
    };
    alloc::vec![mine, String::from("/Applications")]
}

fn read_text(path: &str, max: usize) -> Option<String> {
    let b = crate::fs::read_all(path).ok()?;
    (b.len() <= max).then(|| String::from_utf8(b).ok()).flatten()
}

/// An icon file is only used when it is a small PNG (it is decoded here).
fn icon_for(dir: &str, m: &gina::Manifest) -> Icon {
    if m.icon.is_empty() {
        return Icon::Aurora;
    }
    let path = format!("{dir}/{}", m.icon);
    let ok = crate::fs::read_all(&path).is_ok_and(|b| b.len() <= gina::MAX_ICON && gina::icon_size(&b).is_some());
    if ok {
        icons::custom(&path)
    } else {
        Icon::Aurora
    }
}

fn scan(root: &str, out: &mut Vec<App>) {
    for dir in crate::fs::readdir(root).unwrap_or_default() {
        if !gina::safe_path(&dir.name) || dir.name.contains('/') || dir.name.starts_with('.') {
            continue;
        }
        if out.len() >= 256 || out.iter().any(|a| a.manifest.id == dir.name) {
            continue;
        }
        let app_root = format!("{root}/{}", dir.name);
        let Some(active) = read_text(&format!("{app_root}/active"), 256) else { continue };
        let active = active.trim();
        if !gina::safe_path(active) || active.contains('/') {
            continue;
        }
        let base = format!("{app_root}/{active}");
        let Some(text) = read_text(&format!("{base}/manifest.toml"), 16 * 1024) else { continue };
        let Ok(manifest) = gina::Manifest::parse(&text) else { continue };
        if manifest.id != dir.name {
            continue;
        }
        let path = format!("{base}/{}", manifest.entry);
        if crate::fs::stat(&path).is_ok() {
            let icon = icon_for(&base, &manifest);
            out.push(App { manifest, path, dir: base, icon });
        }
    }
}

pub fn list() -> Vec<App> {
    let mut out = Vec::new();
    for root in roots() {
        scan(&root, &mut out);
    }
    out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    out
}

/// The installed app a running program belongs to.
#[cfg(feature = "ktest")]
pub fn by_program(path: &str) -> Option<App> {
    list().into_iter().find(|a| path.strip_prefix(a.dir.as_str()).is_some_and(|rest| rest.starts_with('/')))
}

/// Apps kept in the dock (the `dock_apps` preference, package ids).
#[cfg(feature = "ktest")]
pub fn pinned() -> Vec<App> {
    let ids = crate::gui::prefs::get("dock_apps").unwrap_or_default();
    let apps = list();
    ids.split(',').filter_map(|id| apps.iter().find(|a| a.manifest.id == id).cloned()).take(16).collect()
}

/// The test server's address, from the harness (None: no server).
#[cfg(feature = "ktest")]
fn test_base() -> Option<String> {
    let port = crate::gui::prefs::boot_option("test_http_port").and_then(|p| p.parse::<u16>().ok())?;
    Some(format!("http://10.0.2.2:{port}/"))
}

/// Starts `pkgtest BASE phase [arg]`.
#[cfg(feature = "ktest")]
fn start_pkgtest(base: &str, phase: &str, arg: &str) -> alloc::sync::Arc<crate::proc::Process> {
    let pid = crate::proc::spawn("/System/Bin/pkgtest", &["pkgtest", base, phase, arg], [None, None, None], 0)
        .expect("spawn pkgtest");
    crate::proc::get(pid).unwrap()
}

/// Runs `pkgtest BASE phase [arg]` to the end; it must pass.
#[cfg(feature = "ktest")]
fn pkgtest(base: &str, phase: &str, arg: &str) {
    let p = start_pkgtest(base, phase, arg);
    let deadline = crate::time::uptime_ms() + 240_000;
    while p.exit_code().is_none() && crate::time::uptime_ms() < deadline {
        crate::sched::sleep_ms(20);
    }
    assert_eq!(p.exit_code(), Some(0), "pkgtest {phase} failed (see log above)");
}

/// Set by `pkgtest` while it writes an install's files (see pkgtest).
#[cfg(feature = "ktest")]
const MARKER: &str = "/pkgtest-installing";

/// `cargo xtask test`: installs, updates and removes apps from the host's
/// test repository with `pkgtest`, and checks the desktop sees each step.
#[cfg(feature = "ktest")]
pub fn self_test() {
    let Some(base) = test_base() else {
        crate::kprint!("[no test server: skipped] ");
        return;
    };
    // The previous start was cut off in the middle of an install (see
    // `power_cut`): nothing may be half-installed.
    if crate::fs::stat(MARKER).is_ok() {
        pkgtest(&base, "powercut-check", "");
        crate::kprint!("[power cut mid-install: recovered] ");
    }
    let run = |phase: &str| pkgtest(&base, phase, "");
    run("install");
    let apps = list();
    let gallery = apps.iter().find(|a| a.manifest.id == "dev.example.gallery").expect("the desktop sees the Gallery");
    assert_eq!(gallery.manifest.version, "1.1.0", "the update is live");
    assert!(matches!(gallery.icon, Icon::Custom(_)), "the Gallery has its own icon");
    let doom = apps.iter().find(|a| a.manifest.id == "org.waveos.doom").expect("the desktop sees Doom");
    assert!(doom.manifest.is_game() && doom.manifest.extensions.iter().any(|e| e == "wad"));
    assert!(pinned().iter().any(|a| a.manifest.id == "org.waveos.doom"), "Doom is pinned to the dock");
    assert!(by_program(&doom.path).is_some_and(|a| a.manifest.id == "org.waveos.doom"));
    run("remove");
    assert!(!list().iter().any(|a| a.manifest.id == "dev.example.gallery" || a.manifest.id == "org.waveos.doom"));
    assert!(pinned().is_empty(), "removal unpins");
    crate::kprint!("[install, update, roll back, remove from the repository] ");
}

/// Catalogue signatures and freshness, key rotation, damaged, cut-short
/// and resumed downloads, pausing and cancelling (`pkgtest store`).
#[cfg(feature = "ktest")]
pub fn store_test() {
    let Some(base) = test_base() else {
        crate::kprint!("[no test server: skipped] ");
        return;
    };
    pkgtest(&base, "store", "");
    crate::kprint!("[rollback, freeze, rotation, damaged, resumed, paused, cancelled] ");
}

/// An install killed half-way: nothing becomes live, the next install
/// clears what it left (`pkgtest slowinstall`, then `recover`).
#[cfg(feature = "ktest")]
pub fn kill_test() {
    let Some(base) = test_base() else {
        crate::kprint!("[no test server: skipped] ");
        return;
    };
    let _ = crate::fs::unlink(MARKER);
    let p = start_pkgtest(&base, "slowinstall", "");
    let deadline = crate::time::uptime_ms() + 120_000;
    while crate::fs::stat(MARKER).is_err() && p.exit_code().is_none() && crate::time::uptime_ms() < deadline {
        crate::sched::sleep_ms(20);
    }
    assert!(p.exit_code().is_none(), "the slow install ended before it could be killed");
    crate::proc::kill(p.pid).expect("kill pkgtest");
    while p.exit_code().is_none() {
        crate::sched::sleep_ms(20);
    }
    pkgtest(&base, "recover", "");
    crate::kprint!("[killed mid-install, recovered] ");
}

/// The version after this one (0.8.0 → 0.8.1), as xtask numbers test updates.
#[cfg(feature = "ktest")]
fn next_version(v: &str) -> String {
    let (head, last) = v.rsplit_once('.').unwrap_or(("0", v));
    format!("{head}.{}", last.parse::<u32>().unwrap_or(0) + 1)
}

/// A whole-system update across starts (with the harness):
///
/// 1. From the EFI partition's system: update to the next version (only
///    the files that changed are downloaded), installed into slot a and
///    armed; the harness starts again.
/// 2. Running slot a on trial: it is the new version; the start is marked
///    good. Then an update whose kernel never starts goes into slot b with
///    one try; the harness starts again, letting the processor reset.
/// 3. That kernel faulted at once, the bootloader ran out of tries and
///    went back to slot a: the failure is reported, nothing else changed.
#[cfg(feature = "ktest")]
pub fn system_test() {
    use crate::gui::sysupdate;
    let Some(base) = test_base() else {
        crate::kprint!("[no test server: skipped] ");
        return;
    };
    if matches!(crate::gui::prefs::boot_option("test_disk").as_deref(), Some("sd" | "emmc")) {
        // The firmware can't read the card the home volume is on.
        crate::kprint!("[slots on an SD card: skipped] ");
        return;
    }
    // In the self-test build the desktop doesn't run: mark this start good
    // as its first frame would.
    sysupdate::mark_good();
    let deadline = crate::time::uptime_ms() + 30_000;
    while sysupdate::outcome().is_none() && crate::time::uptime_ms() < deadline {
        crate::sched::sleep_ms(20);
    }
    let outcome = sysupdate::outcome().expect("the start was not marked");
    let good = next_version(crate::VERSION);
    let broken = next_version(&good);
    let version = crate::fs::read_all("/System/version.txt").unwrap_or_default();
    let version = String::from(String::from_utf8_lossy(&version).lines().next().unwrap_or(""));
    let failed = crate::gui::prefs::boot_option("system_failed");
    match (sysupdate::booted_slot(), failed) {
        ("a", Some(f)) => {
            assert_eq!(f, "b", "the broken update's slot is reported");
            assert!(version.ends_with(&good), "back on {good}: {version}");
            assert!(crate::gui::prefs::boot_option("system_tries").is_none());
            pkgtest(&base, "system-fallback", &broken);
            crate::kprint!("[{broken} never started; back on {good}] ");
        }
        ("esp", _) => {
            assert!(version.ends_with(crate::VERSION), "{version}");
            pkgtest(&base, "system-stage", &good);
            crate::kprint!("[{good} installed in slot a; next start uses it] ");
        }
        ("a", None) => {
            assert!(version.ends_with(&good), "slot a runs {good}: {version}");
            assert!(outcome.contains("marked good"), "{outcome}");
            assert!(crate::gui::prefs::boot_option("system_tries").is_none(), "no tries left pending");
            pkgtest(&base, "system-broken", &broken);
            crate::kprint!("[started {good} from slot a; a broken {broken} armed in slot b] ");
            crate::kprintln!("\nharness: expect fallback");
        }
        (slot, f) => panic!("unexpected start: slot {slot}, failed {f:?} ({outcome})"),
    }
}

/// After the suite on the first start: an install the harness cuts the
/// power during (the next start checks, in `self_test`).
#[cfg(feature = "ktest")]
pub fn power_cut() {
    let Some(base) = test_base() else { return };
    let _ = crate::fs::unlink(MARKER);
    crate::fs::sync_all();
    let p = start_pkgtest(&base, "powercut", "");
    // The harness turns the machine off once pkgtest says so.
    let deadline = crate::time::uptime_ms() + 120_000;
    while p.exit_code().is_none() && crate::time::uptime_ms() < deadline {
        crate::sched::sleep_ms(100);
    }
    crate::kprintln!("power cut: the harness did not cut the power");
}
