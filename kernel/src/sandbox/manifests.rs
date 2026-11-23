//! Which rules a program runs under: the system's own apps' manifests
//! (built in from `userland/apps/*/manifest.toml`), installed apps'
//! manifests (verified by the package manager when installed), or the
//! default profile for everything else.

use super::{Kind, Profile};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::sandbox as cap;

/// What a manifest says, as the sandbox needs it.
#[derive(Clone)]
pub struct AppManifest {
    pub id: String,
    pub name: String,
    pub declared: u64,
    pub kind: Kind,
    pub bundle: Option<String>,
    pub paths: Vec<(String, bool)>,
}

/// The system's own apps: (program, manifest).
const SYSTEM: [(&str, &str); 17] = [
    ("/System/Apps/About.elf", include_str!("../../../userland/apps/about/manifest.toml")),
    ("/System/Apps/Activity.elf", include_str!("../../../userland/apps/activity/manifest.toml")),
    ("/System/Apps/Calculator.elf", include_str!("../../../userland/apps/calculator/manifest.toml")),
    ("/System/Apps/Clock.elf", include_str!("../../../userland/apps/clock/manifest.toml")),
    ("/System/Apps/Doom.elf", include_str!("../../../userland/apps/doom/manifest.toml")),
    ("/System/Apps/Files.elf", include_str!("../../../userland/apps/files/manifest.toml")),
    ("/System/Apps/Firefox.elf", include_str!("../../../userland/apps/firefox/manifest.toml")),
    ("/System/Apps/Installer.elf", include_str!("../../../userland/apps/installer/manifest.toml")),
    ("/System/Apps/Learn.elf", include_str!("../../../userland/apps/learn/manifest.toml")),
    ("/System/Apps/Notes.elf", include_str!("../../../userland/apps/notes/manifest.toml")),
    ("/System/Apps/Paint.elf", include_str!("../../../userland/apps/paint/manifest.toml")),
    ("/System/Apps/Preview.elf", include_str!("../../../userland/apps/preview/manifest.toml")),
    ("/System/Apps/Settings.elf", include_str!("../../../userland/apps/settings/manifest.toml")),
    ("/System/Apps/Studio.elf", include_str!("../../../userland/apps/studio/manifest.toml")),
    ("/System/Apps/Terminal.elf", include_str!("../../../userland/apps/terminal/manifest.toml")),
    ("/System/Apps/Voyage.elf", include_str!("../../../userland/apps/voyage/manifest.toml")),
    ("/System/Apps/Welcome.elf", include_str!("../../../userland/apps/welcome/manifest.toml")),
];

/// Places system apps kept files in before apps had containers: (app,
/// path, writable). Kept so nothing they saved is lost.
const OLD_PLACES: [(&str, &str, bool); 1] = [("org.waveos.clock", "/Settings/clock.conf", true)];

/// The bits of permission names.
pub fn bits(perms: &[String]) -> u64 {
    perms.iter().filter_map(|p| cap::bit(p)).fold(0, |a, b| a | b)
}

/// The permission names of `bits`.
pub fn names(bits: u64) -> Vec<&'static str> {
    cap::NAMES.iter().filter(|(_, b)| bits & b == *b).map(|(n, _)| *n).collect()
}

fn system_app(program: &str, text: &str) -> Option<AppManifest> {
    let m = match gina::Manifest::parse_first_party(text) {
        Ok(m) => m,
        Err(e) => {
            log!("sandbox", "the manifest of {} is not valid: {}", program, e);
            return None;
        }
    };
    let paths = OLD_PLACES.iter().filter(|(id, _, _)| *id == m.id).map(|(_, p, w)| (String::from(*p), *w)).collect();
    Some(AppManifest { declared: bits(&m.permissions), id: m.id, name: m.name, kind: Kind::System, bundle: None, paths })
}

/// Every system app: (program, manifest).
pub fn system_apps() -> Vec<(&'static str, AppManifest)> {
    SYSTEM.iter().filter_map(|(p, t)| system_app(p, t).map(|m| (*p, m))).collect()
}

/// An installed app's rules: a format-2 manifest declares what the app
/// may do; format-1 packages (from before permissions) get the default
/// profile under their own id.
pub fn installed(app: &crate::gui::installed::App) -> AppManifest {
    let m = &app.manifest;
    let (declared, kind) = if m.format >= 2 { (bits(&m.permissions) & !cap::SYSTEM_ONLY, Kind::Package) } else { (cap::ALL, Kind::Legacy) };
    AppManifest {
        id: m.id.clone(),
        name: m.name.clone(),
        declared,
        kind,
        bundle: Some(app.dir.clone()),
        paths: Vec::new(),
    }
}

/// The app `program` belongs to, if it is one.
pub fn for_program(program: &str) -> Option<AppManifest> {
    let program = crate::fs::resolve("/", program);
    if let Some((p, t)) = SYSTEM.iter().find(|(p, _)| *p == program) {
        return system_app(p, t);
    }
    if !(program.starts_with("/Applications/") || program.contains("/Library/Applications/")) {
        return None;
    }
    let apps = crate::fs::perm::system(crate::gui::installed::list);
    apps.iter().find(|a| super::under(&program, &a.dir)).map(installed)
}

/// The default profile, for a program without a manifest: what its
/// account may do, except the guarded devices without asking.
pub fn legacy(program: &str) -> AppManifest {
    let base = program.rsplit('/').next().unwrap_or(program).trim_end_matches(".elf");
    let clean: String =
        base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' }).take(64).collect();
    AppManifest {
        id: format!("legacy.{}", if clean.is_empty() { "program" } else { &clean }),
        name: String::from(base),
        declared: cap::ALL,
        kind: Kind::Legacy,
        bundle: None,
        paths: Vec::new(),
    }
}

/// Every app the person may have decisions about: system apps, installed
/// apps, and programs that asked (by id). (id, name, kind, declared)
pub fn catalog(uid: u32) -> Vec<(String, String, Kind, u64)> {
    let mut out: Vec<(String, String, Kind, u64)> =
        system_apps().into_iter().map(|(_, m)| (m.id, m.name, m.kind, m.declared)).collect();
    for a in crate::fs::perm::system(crate::gui::installed::list) {
        let m = installed(&a);
        if !out.iter().any(|(id, ..)| *id == m.id) {
            out.push((m.id, m.name, m.kind, m.declared));
        }
    }
    for (id, _, _) in super::consent::all(uid) {
        if !out.iter().any(|(i, ..)| *i == id) {
            let name = id.strip_prefix("legacy.").unwrap_or(&id);
            out.push((id.clone(), String::from(name), Kind::Legacy, cap::ALL));
        }
    }
    out
}

impl Profile {
    /// A profile for the self-tests: app `id` declaring `declared`.
    #[cfg(feature = "ktest")]
    pub fn for_test(id: &str, declared: u64, kind: Kind, uid: u32) -> alloc::sync::Arc<Profile> {
        let m = AppManifest {
            id: String::from(id),
            name: String::from(id),
            declared,
            kind,
            bundle: None,
            paths: Vec::new(),
        };
        Profile::new(&m, uid)
    }
}
