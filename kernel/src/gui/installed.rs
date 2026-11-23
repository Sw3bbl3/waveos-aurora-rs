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

/// `cargo xtask test`: installs, updates and removes apps from the host's
/// test repository with `pkgtest`, and checks the desktop sees each step.
#[cfg(feature = "ktest")]
pub fn self_test() {
    let Some(port) = crate::gui::prefs::boot_option("test_http_port").and_then(|p| p.parse::<u16>().ok()) else {
        crate::kprint!("[no test server: skipped] ");
        return;
    };
    let base = format!("http://10.0.2.2:{port}/");
    let run = |phase: &str| {
        let pid = crate::proc::spawn("/System/Bin/pkgtest", &["pkgtest", &base, phase], [None, None, None], 0)
            .expect("spawn pkgtest");
        let p = crate::proc::get(pid).unwrap();
        let deadline = crate::time::uptime_ms() + 180_000;
        while p.exit_code().is_none() && crate::time::uptime_ms() < deadline {
            crate::sched::sleep_ms(20);
        }
        assert_eq!(p.exit_code(), Some(0), "pkgtest {phase} failed (see log above)");
    };
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
    crate::kprint!("[install, update, remove from the repository] ");
}
