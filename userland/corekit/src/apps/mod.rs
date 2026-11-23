//! Installed apps: GINA package installation, removal, verification and
//! discovery, shared by `pkg`, Installer, Settings, Studio and the WaveSDK.
//!
//! # Layout
//!
//! An app lives in its own folder under one of two roots ([`Scope`]):
//! `/Applications` (everyone; it belongs to the `admin` group) or
//! `~/Library/Applications` (one account; the first account's home is the
//! volume root, where `/Applications` already is). In it:
//!
//! ```text
//! <root>/<id>/active                 name of the live generation (one line)
//! <root>/<id>/previous               the generation it replaced (kept for [`rollback`])
//! <root>/<id>/<version>-<sha256>/    a generation: the package's files,
//!     manifest.toml, META/hashes, META/signature, META/receipt
//! <root>/.registry                   every installed app, one line each (derived)
//! ~/Library/Containers/<id>/         the app's data, kept apart from its files
//! ```
//!
//! # Atomic installs
//!
//! A new generation is written to a staging folder, flushed, then renamed
//! into place; only then is `active` replaced, by writing a new pointer,
//! flushing and renaming it over the old one. An install interrupted at any
//! point leaves either the old version or the new one live, never a mix,
//! and the next install or removal clears what was left behind. The
//! generation an update replaced is kept, so [`rollback`] can switch back
//! to it the same way.
//!
//! Privileged steps (anything under `/Applications`, trusting keys, adding
//! repositories) go through [`authorize`].

pub mod download;
pub mod repo;
pub mod trust;

use crate::{fs, process, users};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

pub use gina::Manifest;
pub use trust::{authorize, Trust};
pub type Result<T> = core::result::Result<T, String>;
static INSTALL_LOCK: crate::sync::Mutex<()> = crate::sync::Mutex::new(());

/// Where an app is installed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// `/Applications`, for every account (administrators install there).
    System,
    /// `~/Library/Applications`, for the account installing it.
    User,
}

impl Scope {
    pub fn root(self) -> String {
        match self {
            Scope::System => String::from("/Applications"),
            Scope::User => users::home_folder("Library/Applications"),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Scope::System => "everyone",
            Scope::User => "you",
        }
    }
}

/// How an install should go.
#[derive(Clone, Debug)]
pub struct Options {
    pub scope: Scope,
    /// The person was warned that the package is unsigned or signed by a
    /// key they don't trust, and chose to go on.
    pub allow_untrusted: bool,
    /// Allow replacing a newer installed version with an older one.
    pub allow_downgrade: bool,
    /// Where the package came from (a path or address), for the receipt.
    pub source: String,
}

impl Default for Options {
    fn default() -> Self {
        Self { scope: Scope::System, allow_untrusted: false, allow_downgrade: false, source: String::new() }
    }
}

/// What the installer recorded about an installation (`META/receipt`).
#[derive(Clone, Debug, Default)]
pub struct Receipt {
    pub installed_at: i64,
    /// The signing key (hex) or empty.
    pub signer: String,
    /// The trusted publisher's name, when the key was trusted.
    pub publisher: String,
    pub source: String,
    pub size: u64,
    pub fingerprint: String,
}

impl Receipt {
    fn encode(&self) -> String {
        format!(
            "installed_at={}\nsigner={}\npublisher={}\nsource={}\nsize={}\nfingerprint={}\n",
            self.installed_at, self.signer, self.publisher, self.source, self.size, self.fingerprint
        )
    }
    fn parse(text: &str) -> Self {
        let mut r = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "installed_at" => r.installed_at = v.parse().unwrap_or(0),
                "signer" => r.signer = v.into(),
                "publisher" => r.publisher = v.into(),
                "source" => r.source = v.into(),
                "size" => r.size = v.parse().unwrap_or(0),
                "fingerprint" => r.fingerprint = v.into(),
                _ => {}
            }
        }
        r
    }
}

#[derive(Clone)]
pub struct Installed {
    pub manifest: Manifest,
    pub executable: String,
    pub directory: String,
    pub scope: Scope,
    pub receipt: Receipt,
}

impl Installed {
    /// The icon picture's path, if the app has one.
    pub fn icon(&self) -> Option<String> {
        (!self.manifest.icon.is_empty()).then(|| format!("{}/{}", self.directory, self.manifest.icon))
    }
    /// Where the app keeps its data (for the account asking).
    pub fn data_dir(&self) -> String {
        data_dir(&self.manifest.id)
    }
}

fn io(e: crate::Error) -> String {
    if e.is(crate::abi::err::EACCES) || e.is(crate::abi::err::EPERM) {
        String::from("Permission denied: installing for everyone needs administrator approval")
    } else {
        format!("Filesystem error: {e}")
    }
}

pub(crate) fn mkdirs(path: &str) -> Result<()> {
    let mut current = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        current.push('/');
        current.push_str(part);
        if !fs::is_dir(&current) {
            fs::mkdir(&current).map_err(io)?;
        }
    }
    Ok(())
}

/// The app's data folder: `~/Library/Containers/<id>`.
pub fn data_dir(id: &str) -> String {
    users::home_folder(&format!("Library/Containers/{id}"))
}

/// The running WaveOS version (from `/System/version.txt`).
pub fn os_version() -> String {
    fs::read_to_string("/System/version.txt")
        .ok()
        .and_then(|t| t.lines().next().and_then(|l| l.rsplit(' ').next()).map(String::from))
        .unwrap_or_else(|| String::from("0"))
}

fn valid_name(id: &str) -> bool {
    gina::safe_path(id) && !id.contains('/')
}

/// The app `id` in `scope`.
pub fn installed_in(id: &str, scope: Scope) -> Result<Installed> {
    if !valid_name(id) {
        return Err("Invalid app ID".into());
    }
    let root = format!("{}/{id}", scope.root());
    let active = fs::read_to_string(&format!("{root}/active")).map_err(io)?;
    let active = active.trim();
    if !valid_name(active) {
        return Err("Invalid app registration".into());
    }
    let directory = format!("{root}/{active}");
    let text = fs::read_to_string(&format!("{directory}/manifest.toml")).map_err(io)?;
    let manifest = Manifest::parse(&text).map_err(ToString::to_string)?;
    if manifest.id != id {
        return Err("App registration does not match its manifest".into());
    }
    let receipt = fs::read_to_string(&format!("{directory}/META/receipt")).map(|t| Receipt::parse(&t)).unwrap_or_default();
    Ok(Installed { executable: format!("{directory}/{}", manifest.entry), manifest, directory, scope, receipt })
}

/// The app `id`, installed for this account or for everyone.
pub fn installed(id: &str) -> Result<Installed> {
    installed_in(id, Scope::User).or_else(|_| installed_in(id, Scope::System))
}

/// Every installed app this account can use (its own first).
pub fn list() -> Vec<Installed> {
    let mut out: Vec<Installed> = Vec::new();
    for scope in [Scope::User, Scope::System] {
        for e in fs::read_dir(&scope.root()).unwrap_or_default().into_iter().filter(|e| e.is_dir) {
            if let Ok(a) = installed_in(&e.name, scope) {
                if !out.iter().any(|o| o.manifest.id == a.manifest.id) {
                    out.push(a);
                }
            }
        }
    }
    out.sort_by_key(|a| a.manifest.name.to_lowercase());
    out
}

/// Installed apps in `category` ("game" for Wave Play).
pub fn in_category(category: &str) -> Vec<Installed> {
    list().into_iter().filter(|a| a.manifest.categories.iter().any(|c| c == category)).collect()
}

/// What a package is, and what installing it would mean: shown before installing.
#[derive(Clone)]
pub struct Review {
    pub manifest: Manifest,
    pub trust: Trust,
    pub size: u64,
    pub fingerprint: String,
    /// The version installed now, if any.
    pub current: Option<Installed>,
    /// Reasons it cannot be installed at all (missing dependencies, a newer
    /// system needed, a different publisher's key).
    pub blockers: Vec<String>,
}

impl Review {
    pub fn is_update(&self) -> bool {
        self.current.is_some()
    }
    pub fn is_downgrade(&self) -> bool {
        self.current.as_ref().is_some_and(|c| gina::compare_versions(&self.manifest.version, &c.manifest.version).is_lt())
    }
}

/// Checks a package (structure, hashes, signature) and what installing it
/// would do. Errors mean it is damaged or forged.
pub fn review(bytes: &[u8]) -> Result<Review> {
    let package = gina::Package::parse(bytes).map_err(ToString::to_string)?;
    let signer = package.verify_signature().map_err(ToString::to_string)?;
    let trust = trust::classify(signer);
    let m = package.manifest.clone();
    let current = installed(&m.id).ok();
    let mut blockers = Vec::new();
    if !m.min_os.is_empty() && gina::compare_versions(&os_version(), &m.min_os).is_lt() {
        blockers.push(format!("It needs WaveOS {} or later (this is {}).", m.min_os, os_version()));
    }
    for d in m.dependencies() {
        match installed(&d.id) {
            Ok(a) if d.accepts(&a.manifest.version) => {}
            Ok(a) => blockers.push(format!("It needs {d}; version {} is installed.", a.manifest.version)),
            Err(_) => blockers.push(format!("It needs {d}, which is not installed.")),
        }
    }
    if let Some(c) = &current {
        // Key continuity: an update must come from the same publisher key.
        if !c.receipt.signer.is_empty() && signer.map(|k| gina::hex(&k)) != Some(c.receipt.signer.clone()) {
            blockers.push(String::from(
                "This version is not signed by the publisher of the installed one. Remove the app first if you trust it.",
            ));
        }
    }
    Ok(Review { manifest: m, trust, size: package.installed_size, fingerprint: package.fingerprint, current, blockers })
}

/// Installs with the old defaults (everyone; the caller has already warned
/// about unsigned packages: Studio and `wavesdk` install your own builds).
pub fn install(bytes: &[u8], progress: impl FnMut(usize, usize)) -> Result<Installed> {
    install_with(bytes, &Options { allow_untrusted: true, allow_downgrade: true, ..Options::default() }, progress)
}

/// Owner and mode for installed files: under `/Applications` the system's,
/// with the admin group allowed to change them; executables runnable.
fn set_modes(path: &str, system: bool) -> Result<()> {
    let dir = fs::is_dir(path);
    let elf = !dir && fs::File::open(path, crate::abi::open::READ).ok().and_then(|mut f| {
        let mut magic = [0u8; 4];
        f.read(&mut magic).ok().map(|n| n == 4 && magic == *b"\x7fELF")
    }) == Some(true);
    if system {
        users::chown(path, crate::abi::cred::ROOT_UID, crate::abi::cred::ADMIN_GID).map_err(io)?;
    }
    let mode = match (dir, elf, system) {
        (true, _, true) => 0o775,
        (true, _, false) | (false, true, _) => 0o755,
        (false, false, _) => 0o644,
    };
    users::chmod(path, mode).map_err(io)?;
    if dir {
        for e in fs::read_dir(path).map_err(io)? {
            set_modes(&fs::join(path, &e.name), system)?;
        }
    }
    Ok(())
}

/// Removes generations and staging folders other than the live one and
/// the one it replaced.
fn collect_garbage(root: &str) {
    let active = fs::read_to_string(&format!("{root}/active")).unwrap_or_default();
    let previous = fs::read_to_string(&format!("{root}/previous")).unwrap_or_default();
    let live: Vec<u32> = process::list().iter().map(|p| p.pid).collect();
    // Process numbers start again with each start of the computer: what an
    // installer left before this start is never someone's work in progress.
    let booted = crate::time::boot_file_time().saturating_sub(2);
    for e in fs::read_dir(root).unwrap_or_default() {
        // Another installer's work in progress is left alone.
        let path = format!("{root}/{}", e.name);
        let busy = e
            .name
            .strip_prefix(".pending-")
            .or_else(|| e.name.strip_prefix(".active-"))
            .or_else(|| e.name.strip_prefix(".previous-"))
            .and_then(|p| p.parse::<u32>().ok())
            .is_some_and(|pid| {
                pid != process::pid() && live.contains(&pid) && fs::stat(&path).is_ok_and(|s| s.mtime >= booted)
            });
        let kept = e.name == "active" || e.name == "previous" || e.name == active.trim() || e.name == previous.trim();
        if !kept && !busy {
            let _ = fs::remove_all(&path);
        }
    }
}

/// Installs a package. The live version changes only after every file of
/// the new one is on disk.
pub fn install_with(bytes: &[u8], opts: &Options, mut progress: impl FnMut(usize, usize)) -> Result<Installed> {
    let _install = INSTALL_LOCK.lock();
    let review = review(bytes)?;
    if let Some(b) = review.blockers.first() {
        return Err(b.clone());
    }
    if !opts.allow_untrusted {
        match &review.trust {
            Trust::Trusted(_) => {}
            Trust::Unknown(k) => {
                return Err(format!(
                    "The package is signed by a key you don't trust ({}). Install it only if you trust its source.",
                    gina::fingerprint(k)
                ))
            }
            Trust::Unsigned => {
                return Err(String::from(
                    "The package is not signed, so nobody vouches for it. Install it only if you trust its source.",
                ))
            }
        }
    }
    if review.is_downgrade() && !opts.allow_downgrade {
        return Err(format!(
            "A newer version ({}) is installed.",
            review.current.as_ref().map(|c| c.manifest.version.as_str()).unwrap_or("")
        ));
    }
    let id = review.manifest.id.clone();
    let scope = match &review.current {
        Some(c) if c.scope != opts.scope => {
            return Err(format!("{} is already installed for {}; remove it first.", review.manifest.name, c.scope.name()))
        }
        Some(c) => c.scope,
        None => opts.scope,
    };
    let system = scope == Scope::System;
    if system {
        authorize(&format!("install {} for everyone", review.manifest.name))?;
    }
    let package = gina::Package::parse(bytes).map_err(ToString::to_string)?;
    let root = format!("{}/{id}", scope.root());
    mkdirs(&scope.root())?;
    if !fs::is_dir(&root) {
        fs::mkdir(&root).map_err(io)?;
        set_modes(&root, system)?;
    }
    let generation = format!("{}-{}", package.manifest.version, package.fingerprint);
    let directory = format!("{root}/{generation}");
    // A staged generation cannot alter a running or previously installed app.
    let staging = format!("{root}/.pending-{}", process::pid());
    if fs::exists(&staging) {
        fs::remove_all(&staging).map_err(io)?;
    }
    let (signer, publisher) = match &review.trust {
        Trust::Trusted(name) => (package.signature.map(|s| gina::hex(&s.key)).unwrap_or_default(), name.clone()),
        Trust::Unknown(k) => (gina::hex(k), String::new()),
        Trust::Unsigned => (String::new(), String::new()),
    };
    let receipt = Receipt {
        installed_at: (crate::time::unix_ms() / 1000.0) as i64,
        signer,
        publisher,
        source: opts.source.chars().filter(|c| !c.is_control()).collect(),
        size: package.installed_size,
        fingerprint: package.fingerprint.clone(),
    };
    let result = (|| {
        fs::mkdir(&staging).map_err(io)?;
        let total = package.files.len() + 1;
        for (i, file) in package.files.iter().enumerate() {
            let path = format!("{staging}/{}", file.path);
            if let Some((parent, _)) = path.rsplit_once('/') {
                mkdirs(parent)?;
            }
            fs::write(&path, file.bytes).map_err(io)?;
            progress(i + 1, total);
        }
        mkdirs(&format!("{staging}/META"))?;
        fs::write(&format!("{staging}/META/receipt"), receipt.encode().as_bytes()).map_err(io)?;
        set_modes(&staging, system)?;
        fs::try_sync_path(&staging).map_err(io)?;
        if !fs::exists(&directory) {
            fs::rename(&staging, &directory).map_err(io)?;
        } else {
            // A matching generation name alone is not proof that its files
            // survived storage corruption or were not modified after install.
            for file in &package.files {
                if fs::read(&format!("{directory}/{}", file.path)).map_err(io)? != file.bytes {
                    return Err(
                        "The existing package generation is damaged. The active registration was preserved.".into()
                    );
                }
            }
            fs::remove_all(&staging).map_err(io)?;
        }
        // The version being replaced stays, for `rollback`.
        let old = fs::read_to_string(&format!("{root}/active")).map(|s| String::from(s.trim())).unwrap_or_default();
        if !old.is_empty() && old != generation && fs::is_dir(&format!("{root}/{old}")) {
            set_pointer(&root, "previous", &old, system)?;
        }
        set_pointer(&root, "active", &generation, system)?;
        progress(total, total);
        installed_in(&id, scope)
    })();
    if result.is_err() {
        let _ = fs::remove_all(&staging);
    } else {
        collect_garbage(&root);
        let _ = mkdirs(&data_dir(&id));
        let _ = write_registry(scope);
        refresh();
    }
    result
}

/// Points `<root>/<name>` at generation `to`: a new file, flushed, renamed
/// over the old one.
fn set_pointer(root: &str, name: &str, to: &str, system: bool) -> Result<()> {
    let pending = format!("{root}/.{name}-{}", process::pid());
    fs::write(&pending, to.as_bytes()).map_err(io)?;
    set_modes(&pending, system)?;
    fs::try_sync_path(&pending).map_err(io)?;
    fs::rename(&pending, &format!("{root}/{name}")).map_err(io)?;
    fs::try_sync_path(root).map_err(io)
}

/// The version an update replaced, if it is still there to go back to.
pub fn previous_version(id: &str) -> Option<String> {
    let app = installed(id).ok()?;
    let root = format!("{}/{id}", app.scope.root());
    let previous = fs::read_to_string(&format!("{root}/previous")).ok()?;
    let previous = previous.trim();
    if !valid_name(previous) || app.directory.ends_with(&format!("/{previous}")) {
        return None;
    }
    let text = fs::read_to_string(&format!("{root}/{previous}/manifest.toml")).ok()?;
    Manifest::parse(&text).ok().filter(|m| m.id == id).map(|m| m.version)
}

/// Goes back to the version an update replaced (its files are checked
/// against its hash list first). The app's data is left as it is.
pub fn rollback(id: &str) -> Result<Installed> {
    let _install = INSTALL_LOCK.lock();
    let app = installed(id)?;
    let root = format!("{}/{id}", app.scope.root());
    let previous = fs::read_to_string(&format!("{root}/previous")).map_err(|_| String::from("There is no earlier version to go back to."))?;
    let previous = String::from(previous.trim());
    let dir = format!("{root}/{previous}");
    if !valid_name(&previous) || !fs::is_dir(&dir) || app.directory == dir {
        return Err("There is no earlier version to go back to.".into());
    }
    let hashes = fs::read_to_string(&format!("{dir}/{}", gina::HASHES)).map_err(|_| String::from("The earlier version has no hash list."))?;
    let bad = gina::verify_files(&hashes, |p| fs::read(&format!("{dir}/{p}")).ok()).map_err(ToString::to_string)?;
    if !bad.is_empty() {
        return Err(format!("The earlier version is damaged ({} files changed or missing).", bad.len()));
    }
    let system = app.scope == Scope::System;
    if system {
        authorize(&format!("change {} for everyone", app.manifest.name))?;
    }
    let current = String::from(fs::file_name(&app.directory));
    set_pointer(&root, "active", &previous, system)?;
    set_pointer(&root, "previous", &current, system)?;
    let _ = write_registry(app.scope);
    refresh();
    installed_in(id, app.scope)
}

/// Clears what interrupted installs left behind (staging folders of
/// installers no longer running) wherever this account may write.
pub fn recover() {
    for scope in [Scope::User, Scope::System] {
        if scope == Scope::System && !users::is_admin() {
            continue;
        }
        for e in fs::read_dir(&scope.root()).unwrap_or_default().into_iter().filter(|e| e.is_dir && !e.name.starts_with('.')) {
            let root = format!("{}/{}", scope.root(), e.name);
            if fs::read_dir(&root).unwrap_or_default().iter().any(|f| f.name.starts_with('.')) {
                collect_garbage(&root);
            }
        }
    }
}

/// Removes an app. Its data stays unless `delete_data` (only this account's
/// data can be removed; other accounts keep theirs).
pub fn uninstall(id: &str, delete_data: bool) -> Result<()> {
    let _install = INSTALL_LOCK.lock();
    let app = installed(id)?;
    if app.scope == Scope::System {
        authorize(&format!("remove {} for everyone", app.manifest.name))?;
    }
    // Deactivate first so interrupted cleanup cannot leave a launchable half-app.
    let root = format!("{}/{id}", app.scope.root());
    fs::remove(&format!("{root}/active")).map_err(io)?;
    refresh();
    fs::remove_all(&root).map_err(io)?;
    let data = data_dir(id);
    if delete_data && fs::exists(&data) {
        fs::remove_all(&data).map_err(io)?;
    }
    let _ = set_pinned(id, false);
    let _ = write_registry(app.scope);
    fs::try_sync().map_err(io)?;
    refresh();
    Ok(())
}

/// Re-hashes an installed app's files against the list it was installed
/// with. Returns the files that are missing or changed.
pub fn verify(id: &str) -> Result<Vec<String>> {
    let app = installed(id)?;
    let hashes = fs::read_to_string(&format!("{}/{}", app.directory, gina::HASHES))
        .map_err(|_| String::from("This app was installed from a format 1 package, which has no hash list."))?;
    gina::verify_files(&hashes, |p| fs::read(&format!("{}/{p}", app.directory)).ok()).map_err(ToString::to_string)
}

pub fn launch(id: &str) -> Result<u32> {
    let app = installed(id)?;
    process::spawn(&app.executable, &[], process::Stdio::default()).map_err(io)
}

/// The registry file for `scope`: one line per installed app, for programs
/// (such as Wave Play) that want the list without parsing every manifest.
pub fn registry_path(scope: Scope) -> String {
    format!("{}/.registry", scope.root())
}

/// `gina-registry 1`, then per app, tab-separated: `app`, id, version,
/// name, categories (comma-separated), executable, icon, directory,
/// extensions, size, publisher.
pub fn registry_text(scope: Scope) -> String {
    let mut out = String::from("gina-registry 1\n");
    let mut apps: Vec<Installed> = fs::read_dir(&scope.root())
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.is_dir)
        .filter_map(|e| installed_in(&e.name, scope).ok())
        .collect();
    apps.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    for a in apps {
        let m = &a.manifest;
        out += &format!(
            "app\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            m.id,
            m.version,
            m.name,
            m.categories.join(","),
            a.executable,
            a.icon().unwrap_or_default(),
            a.directory,
            m.extensions.join(","),
            a.receipt.size,
            a.receipt.publisher
        );
    }
    out
}

/// Rewrites the registry atomically (new file, flush, rename).
pub fn write_registry(scope: Scope) -> Result<()> {
    let path = registry_path(scope);
    let tmp = format!("{path}.new-{}", process::pid());
    fs::write(&tmp, registry_text(scope).as_bytes()).map_err(io)?;
    // Under /Applications only administrators may change it.
    if scope == Scope::System {
        users::chown(&tmp, crate::abi::cred::ROOT_UID, crate::abi::cred::ADMIN_GID).map_err(io)?;
        users::chmod(&tmp, 0o644).map_err(io)?;
    }
    fs::try_sync_path(&tmp).map_err(io)?;
    fs::rename(&tmp, &path).map_err(io)
}

/// Package ids kept in the dock (the `dock_apps` preference).
pub fn pinned() -> Vec<String> {
    crate::prefs::get("dock_apps")
        .unwrap_or_default()
        .split(',')
        .filter(|s| gina::valid_id(s))
        .map(String::from)
        .collect()
}

pub fn set_pinned(id: &str, pin: bool) -> Result<()> {
    let mut ids = pinned();
    ids.retain(|i| i != id);
    if pin {
        installed(id)?;
        ids.push(id.into());
    }
    crate::prefs::set("dock_apps", &ids.join(",")).map_err(io)?;
    refresh();
    Ok(())
}

/// Tells the desktop the installed apps changed (launcher, search, dock).
pub fn refresh() {
    let _ = crate::sys::call(crate::abi::nr::DESKTOP, &[crate::abi::desktop::REFRESH_APPS as u64, 0, 0]);
}
