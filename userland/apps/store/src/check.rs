//! `Store.elf --check`: the background check the desktop starts now and
//! then. At most once a day, it looks at the repositories (only when some
//! are configured and reachable), and posts one notification for updates
//! not mentioned before — apps and WaveOS itself. Clicking it opens the
//! Store on its Updates. With "Download updates automatically" on
//! (`software_update_download`), a system update is downloaded too (it is
//! installed only when someone chooses to).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use corekit::apps::download::Control;
use corekit::apps::repo;
use corekit::{fs, prefs, update, users};

/// Checks at most this often (seconds).
const EVERY: u64 = 20 * 3600;

fn state_path() -> String {
    users::home_folder("Library/Packages/check")
}

/// Where the Store learns it was opened from an update notification.
pub fn hint_path() -> String {
    users::home_folder("Library/Packages/open-updates")
}

fn now() -> u64 {
    (corekit::time::unix_ms() / 1000.0) as u64
}

pub fn run(force: bool) -> i32 {
    let sources = repo::sources();
    if sources.is_empty() {
        return 0;
    }
    let text = fs::read_to_string(&state_path()).unwrap_or_default();
    let mut lines = text.lines();
    let last: u64 = lines.next().and_then(|l| l.parse().ok()).unwrap_or(0);
    let told: Vec<String> = lines.map(String::from).collect();
    if !force && now().saturating_sub(last) < EVERY {
        return 0;
    }
    // Only what the network says now; cached catalogues have nothing new.
    let catalogs: Vec<repo::Catalog> = sources.iter().filter_map(|s| repo::catalog(s).ok()).collect();
    if catalogs.is_empty() {
        return 0;
    }
    let _ = fs::mkdir(&users::home_folder("Library"));
    let _ = fs::mkdir(&users::home_folder("Library/Packages"));
    let mut found: Vec<(String, String)> = repo::updates_in(&catalogs)
        .into_iter()
        .map(|(a, _, e)| (format!("{} {}", a.manifest.id, e.version), a.manifest.name.clone()))
        .collect();
    let mut system = None;
    if let Ok(Some(av)) = update::check(None) {
        if prefs::get_bool("software_update_download") {
            let _ = update::download(&av, &Control::new(), |_, _| {});
        }
        found.insert(0, (format!("system {}", av.update.version), format!("WaveOS {}", av.update.version)));
        system = Some(av.update.version.clone());
    }
    let new: Vec<&(String, String)> = found.iter().filter(|(k, _)| !told.contains(k)).collect();
    if !new.is_empty() {
        let apps = found.len() - system.is_some() as usize;
        let body = match (&system, apps) {
            (Some(v), 0) => format!("WaveOS {v} is ready to install."),
            (Some(v), n) => format!("WaveOS {v} and {n} app update{} are ready.", if n == 1 { "" } else { "s" }),
            (None, 1) => format!("An update for {} is ready.", found[0].1),
            (None, n) => format!("{n} app updates are ready."),
        };
        corekit::notify::post("Updates Available", &body);
        let _ = fs::write_atomic(&hint_path(), format!("{}\n", now()).as_bytes());
    }
    let mut out = format!("{}\n", now());
    for (k, _) in &found {
        out += k;
        out.push('\n');
    }
    let _ = fs::write_atomic(&state_path(), out.as_bytes());
    0
}

/// Whether the Store was opened from an update notification (recently).
pub fn opened_for_updates() -> bool {
    let path = hint_path();
    let at: u64 = fs::read_to_string(&path).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
    let _ = fs::remove(&path);
    at > 0 && now().saturating_sub(at) < 30 * 60
}
