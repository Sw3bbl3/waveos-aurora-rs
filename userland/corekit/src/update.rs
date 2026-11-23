//! Whole-system updates (Settings → Software Update, `pkg system`).
//!
//! The system — kernel, bootloader and the read-only system image mounted
//! at `/System` — is replaced as a unit, never file by file. There are
//! three places a system can start from ("slots"):
//!
//! ```text
//! esp   \aurora\kernel.elf + system.tar on the EFI partition (/Boot): as installed
//! a, b  /.system/a/, /.system/b/ on the home volume: kernel.elf, system.tar,
//!       BOOTX64.EFI, update.txt and update.sig of an update
//! ```
//!
//! An update is written to the slot that isn't running, verified by
//! reading every file back, and marked ready (`/.system/ready`). Restarting
//! to update switches `\aurora\boot.conf` to it with a number of tries:
//!
//! ```text
//! system=a              the slot to start
//! system_tries=3        starts left before giving up on it
//! system_previous=esp   where to go back to
//! system_failed=b       (set by the bootloader when it went back)
//! ```
//!
//! The bootloader takes a try before each start of a slot on trial; the
//! desktop marks the start good once it is up (the kernel removes
//! `system_tries`). A slot that runs out of tries — or whose files are
//! missing or don't match `update.txt` — is abandoned and the previous one
//! started instead, so an interrupted or broken update never leaves the
//! computer unable to start. The home volume and its files are never part
//! of an update.
//!
//! Updates come from the same repositories as apps (`system` lines in a
//! catalogue), signed by a system update key in `/System/Keys/Updates`
//! (never an app publisher's). Only files that changed are downloaded: the
//! system image is rebuilt from the running system's files and the new
//! ones (see `gina::system`). Downloading needs no privileges (it goes to
//! `~/Library/Caches/System Update`); installing and restarting need an
//! administrator.
use crate::apps::download::{self, Control, Outcome, Want};
use crate::apps::repo::{self, Catalog};
use crate::apps::{authorize, Result};
use crate::{fs, users};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use gina::repo::SystemRelease;
use gina::system::{self, Item, Update};
use sha2::{Digest, Sha256};

pub const SLOTS: &str = "/.system";
pub const BOOT_CONF: &str = "/Boot/aurora/boot.conf";
pub const UPDATE_KEYS: &str = "/System/Keys/Updates";
/// Starts a new system gets before the previous one is started instead.
pub const TRIES: u32 = 3;

fn io(e: crate::Error) -> String {
    if e.is(crate::abi::err::EACCES) || e.is(crate::abi::err::EPERM) {
        String::from("Permission denied: installing a system update needs administrator approval")
    } else {
        format!("Filesystem error: {e}")
    }
}

/// The slot the running system started from: `esp`, `a` or `b`.
pub fn booted_slot() -> String {
    crate::prefs::get("system_slot").filter(|s| matches!(s.as_str(), "esp" | "a" | "b")).unwrap_or_else(|| "esp".into())
}

fn slot_dir(slot: &str) -> String {
    format!("{SLOTS}/{slot}")
}

/// Where the parts of `slot` are: (kernel, system image, update.txt).
fn slot_files(slot: &str) -> (String, String, String) {
    if slot == "esp" {
        ("/Boot/aurora/kernel.elf".into(), "/Boot/aurora/system.tar".into(), "/Boot/aurora/update.txt".into())
    } else {
        let d = slot_dir(slot);
        (format!("{d}/kernel.elf"), format!("{d}/system.tar"), format!("{d}/update.txt"))
    }
}

/// `key=value` options of `\aurora\boot.conf`.
pub fn boot_options() -> Vec<(String, String)> {
    fs::read_to_string(BOOT_CONF)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect()
}

fn boot_option(key: &str) -> Option<String> {
    boot_options().into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Rewrites boot.conf with `set` (None removes), keeping everything else:
/// a new file, flushed, renamed over the old one.
fn edit_boot_conf(set: &[(&str, Option<&str>)]) -> Result<()> {
    let old = fs::read_to_string(BOOT_CONF).unwrap_or_default();
    let mut text = String::new();
    for line in old.lines() {
        let key = line.trim().split_once('=').map(|(k, _)| k.trim());
        if key.is_none_or(|k| !set.iter().any(|(s, _)| *s == k)) {
            text.push_str(line);
            text.push('\n');
        }
    }
    for (k, v) in set {
        if let Some(v) = v {
            text.push_str(&format!("{k}={v}\n"));
        }
    }
    let tmp = "/Boot/aurora/boot.new";
    fs::write(tmp, text.as_bytes()).map_err(io)?;
    fs::try_sync_path(tmp).map_err(io)?;
    if fs::rename(tmp, BOOT_CONF).is_err() {
        // A filesystem that won't rename over a file: replace it in two steps.
        fs::remove(BOOT_CONF).map_err(io)?;
        fs::rename(tmp, BOOT_CONF).map_err(io)?;
    }
    fs::try_sync_path(BOOT_CONF).map_err(io)
}

/// What the system update machinery knows now.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// The running version.
    pub version: String,
    /// The slot it started from.
    pub slot: String,
    /// This start is still on trial (not yet marked good).
    pub trial: bool,
    /// An update installed and ready for a restart: (slot, version).
    pub ready: Option<(String, String)>,
    /// A version that failed to start, so the previous one was started.
    pub failed: Option<String>,
}

fn slot_version(slot: &str) -> Option<String> {
    Update::parse(&fs::read(&slot_files(slot).2).ok()?).ok().map(|u| u.version)
}

pub fn status() -> Status {
    let ready = fs::read_to_string(&format!("{SLOTS}/ready")).ok().and_then(|t| {
        let (slot, version) = t.trim().split_once('\t')?;
        (matches!(slot, "a" | "b") && fs::is_dir(&slot_dir(slot))).then(|| (slot.to_string(), version.to_string()))
    });
    let failed = boot_option("system_failed").map(|s| slot_version(&s).unwrap_or(s));
    Status {
        version: crate::apps::os_version(),
        slot: booted_slot(),
        trial: boot_option("system_tries").is_some(),
        ready,
        failed,
    }
}

/// The keys system updates are trusted from.
pub fn keys() -> Vec<[u8; 32]> {
    fs::read_dir(UPDATE_KEYS)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| !e.is_dir && e.name.ends_with(".pub") && e.size < 4096)
        .filter_map(|e| fs::read_to_string(&fs::join(UPDATE_KEYS, &e.name)).ok())
        .filter_map(|t| gina::PublicKey::parse(&t).ok().map(|k| k.key))
        .collect()
}

/// A newer system a repository offers, its description verified.
#[derive(Clone)]
pub struct Available {
    pub catalog: Catalog,
    pub release: SystemRelease,
    pub update: Update,
    /// Objects to download: (hash, size).
    pub missing: Vec<([u8; 32], u64)>,
}

impl Available {
    /// Bytes still to download.
    pub fn download_size(&self) -> u64 {
        self.missing.iter().filter(|(s, n)| !download::verified(&object_cache(s), *n, s)).map(|(_, n)| n).sum()
    }
    /// Bytes the new system takes once installed.
    pub fn installed_size(&self) -> u64 {
        self.update.system.size + self.update.kernel.size + self.update.bootloader.size
    }
}

/// Where downloaded objects wait to be installed.
pub fn cache_dir() -> String {
    users::home_folder("Library/Caches/System Update")
}

fn object_cache(sha: &[u8; 32]) -> String {
    format!("{}/{}", cache_dir(), gina::hex(sha))
}

/// The running system's description and where each of its files is:
/// hash → path to read it from.
fn running_files() -> Vec<([u8; 32], String)> {
    let slot = booted_slot();
    let (kernel, _, desc) = slot_files(&slot);
    let mut out = Vec::new();
    if let Some(u) = fs::read(&desc).ok().and_then(|b| Update::parse(&b).ok()) {
        out.push((u.kernel.sha256, kernel));
        out.push((u.bootloader.sha256, String::from("/Boot/EFI/BOOT/BOOTX64.EFI")));
        for item in &u.layout {
            if let Item::File { path, sha256, size } = item {
                if *size > 0 {
                    out.push((*sha256, format!("/System/{path}")));
                }
            }
        }
    }
    out
}

/// Fetches and verifies `release`'s description from `catalog`.
fn describe(catalog: &Catalog, release: &SystemRelease) -> Result<Update> {
    let bases = catalog.bases();
    let dir = cache_dir();
    crate::apps::mkdirs(&dir)?;
    let text_path = format!("{dir}/update-{}.txt", release.version);
    let want = Want { bases: &bases, path: &release.path, size: release.size, sha256: release.sha256 };
    if download::fetch(&want, &text_path, &Control::new(), &mut |_, _| {})? != Outcome::Done {
        return Err("stopped".into());
    }
    let text = fs::read(&text_path).map_err(io)?;
    let sig_path = release.path.trim_end_matches("update.txt").to_string() + "update.sig";
    let sig = get_small(&bases, &sig_path)?;
    let sigs = core::str::from_utf8(&sig)
        .map_err(|_| String::from("Invalid update signature"))
        .and_then(|s| gina::repo::parse_signatures(s).map_err(ToString::to_string))?;
    if gina::sign::verify_any(&sigs, &system::signed_message(&text), &keys()).is_none() {
        return Err(format!(
            "System update {} is not signed by a key this computer trusts for system updates; refusing it.",
            release.version
        ));
    }
    let u = Update::parse(&text).map_err(ToString::to_string)?;
    if u.version != release.version {
        return Err("The system update does not match its catalogue entry.".into());
    }
    Ok(u)
}

fn get_small(bases: &[String], path: &str) -> Result<Vec<u8>> {
    for base in bases {
        if let Some(dir) = base.strip_prefix("file://") {
            if let Ok(b) = fs::read(&format!("{dir}{path}")) {
                return Ok(b);
            }
            continue;
        }
        if let Some(u) = nebula_web::Url::parse(&format!("{base}{path}")) {
            if let Ok(r) = crate::web::fetch(&u) {
                if r.status == 200 && r.body.len() <= 4096 {
                    return Ok(r.body);
                }
            }
        }
    }
    Err(format!("Could not download {path}"))
}

/// Looks for a newer system in the configured repositories (`version`:
/// that one exactly, else the newest that can update this one).
pub fn check(version: Option<&str>) -> Result<Option<Available>> {
    let current = crate::apps::os_version();
    let mut best: Option<(Catalog, SystemRelease)> = None;
    let mut errors = Vec::new();
    for (_, c) in repo::catalogs() {
        let c = match c {
            Ok(c) => c,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let r = match version {
            Some(v) => c.index.system.iter().find(|s| s.version == v).cloned(),
            None => c.index.system_update(&current).cloned(),
        };
        if let Some(r) = r {
            if best.as_ref().is_none_or(|(_, b)| gina::compare_versions(&r.version, &b.version).is_gt()) {
                best = Some((c, r));
            }
        }
    }
    let Some((catalog, release)) = best else {
        if !errors.is_empty() && repo::sources().len() == errors.len() {
            return Err(errors.join("\n"));
        }
        return Ok(None);
    };
    let update = describe(&catalog, &release)?;
    if !update.min_from.is_empty() && gina::compare_versions(&current, &update.min_from).is_lt() {
        return Err(format!("WaveOS {} can't be updated from this version ({current}).", update.version));
    }
    let have = running_files();
    let missing = update.missing(|s| have.iter().any(|(h, _)| h == s));
    Ok(Some(Available { catalog, release, update, missing }))
}

/// Downloads what `av` needs (resumable; pausable and cancellable through
/// `control`). `progress` gets (bytes so far, total).
pub fn download(av: &Available, control: &Control, mut progress: impl FnMut(u64, u64)) -> Result<Outcome> {
    let dir = cache_dir();
    crate::apps::mkdirs(&dir)?;
    let total: u64 = av.missing.iter().map(|(_, n)| n).sum();
    let mut done = 0u64;
    let bases = av.catalog.bases();
    for (sha, size) in &av.missing {
        let path = object_cache(sha);
        let object = system::object_path(sha);
        let want = Want { bases: &bases, path: &object, size: *size, sha256: *sha };
        let outcome = download::fetch(&want, &path, control, &mut |n, _| progress(done + n, total))?;
        if outcome != Outcome::Done {
            return Ok(outcome);
        }
        done += size;
    }
    progress(total, total);
    Ok(Outcome::Done)
}

/// The slot an update goes to: the one not running.
fn target_slot() -> &'static str {
    if booted_slot() == "a" {
        "b"
    } else {
        "a"
    }
}

/// Reads a file and checks its hash.
fn read_checked(path: &str, sha: &[u8; 32]) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    (<[u8; 32]>::from(Sha256::digest(&bytes)) == *sha).then_some(bytes)
}

/// Installs a downloaded update into the slot that isn't running, checks
/// every file by reading it back, and marks it ready. Nothing changes how
/// the computer starts until [`restart_to_update`]. `progress` gets
/// (bytes written, total).
pub fn install(av: &Available, mut progress: impl FnMut(u64, u64)) -> Result<String> {
    let st = status();
    if st.trial {
        return Err("This start of WaveOS hasn't finished yet; try again in a moment.".into());
    }
    authorize(&format!("install WaveOS {}", av.update.version))?;
    if !fs::is_dir(SLOTS) {
        return Err(format!("{SLOTS} is missing; it is made when WaveOS starts."));
    }
    let u = &av.update;
    let need = av.installed_size() + (8 << 20);
    if let Ok((_, free)) = fs::space("/") {
        if free < need {
            return Err(format!("Not enough free space: the update needs {} MB.", need >> 20));
        }
    }
    let running = running_files();
    // A file's bytes: from the running system when it has them, else downloaded.
    let object = |sha: &[u8; 32]| -> Result<Vec<u8>> {
        running
            .iter()
            .filter(|(h, _)| h == sha)
            .find_map(|(_, p)| read_checked(p, sha))
            .or_else(|| read_checked(&object_cache(sha), sha))
            .ok_or_else(|| format!("A file of the update is missing ({}); download it again.", gina::hex(&sha[..6])))
    };
    let slot = target_slot();
    let staging = format!("{SLOTS}/.staging-{}", crate::process::pid());
    if fs::exists(&staging) {
        fs::remove_all(&staging).map_err(io)?;
    }
    fs::mkdir(&staging).map_err(io)?;
    let total = av.installed_size();
    let mut written = 0u64;
    let result = (|| -> Result<()> {
        for (name, part) in [("kernel.elf", &u.kernel), ("BOOTX64.EFI", &u.bootloader)] {
            let bytes = object(&part.sha256)?;
            fs::write(&format!("{staging}/{name}"), &bytes).map_err(io)?;
            written += part.size;
            progress(written, total);
        }
        // The system image, rebuilt entry by entry and hashed as it goes.
        let image = format!("{staging}/system.tar");
        let mut f =
            fs::File::open(&image, crate::abi::open::WRITE | crate::abi::open::CREATE | crate::abi::open::TRUNCATE)
                .map_err(io)?;
        let mut hasher = Sha256::new();
        let put = |f: &mut fs::File, hasher: &mut Sha256, data: &[u8]| -> Result<()> {
            hasher.update(data);
            let mut at = 0;
            while at < data.len() {
                at += f.write(&data[at..]).map_err(io)?;
            }
            Ok(())
        };
        let zeros = [0u8; 1024];
        for item in &u.layout {
            match item {
                Item::Dir(p) => put(&mut f, &mut hasher, &system::tar_header(p, 0, true))?,
                Item::File { path, sha256, size } => {
                    put(&mut f, &mut hasher, &system::tar_header(path, *size, false))?;
                    if *size > 0 {
                        put(&mut f, &mut hasher, &object(sha256)?)?;
                    }
                    put(&mut f, &mut hasher, &zeros[..system::padding(*size)])?;
                    written += size;
                    progress(written.min(total), total);
                }
            }
        }
        put(&mut f, &mut hasher, &zeros)?;
        drop(f);
        if <[u8; 32]>::from(hasher.finalize()) != u.system.sha256 {
            return Err("The rebuilt system image does not match the update.".into());
        }
        let text = u.encode();
        fs::write(&format!("{staging}/update.txt"), text.as_bytes()).map_err(io)?;
        fs::try_sync_path(&staging).map_err(io)?;
        fs::try_sync().map_err(io)?;
        // Read everything back before anything points at it.
        for (name, part) in [("kernel.elf", &u.kernel), ("BOOTX64.EFI", &u.bootloader), ("system.tar", &u.system)] {
            if !download::verified(&format!("{staging}/{name}"), part.size, &part.sha256) {
                return Err(format!("{name} did not read back as written: the disk may be failing."));
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_all(&staging);
        return Err(e);
    }
    let dir = slot_dir(slot);
    let _ = fs::remove(&format!("{SLOTS}/ready"));
    if fs::exists(&dir) {
        fs::remove_all(&dir).map_err(io)?;
    }
    fs::rename(&staging, &dir).map_err(io)?;
    fs::write(&format!("{SLOTS}/ready.new"), format!("{slot}\t{}\n", u.version).as_bytes()).map_err(io)?;
    fs::try_sync_path(&format!("{SLOTS}/ready.new")).map_err(io)?;
    fs::rename(&format!("{SLOTS}/ready.new"), &format!("{SLOTS}/ready")).map_err(io)?;
    fs::try_sync().map_err(io)?;
    // The downloads are no longer needed.
    for (sha, _) in &av.missing {
        let _ = fs::remove(&object_cache(sha));
    }
    Ok(slot.to_string())
}

/// Makes the next start use the ready update, with `tries` starts before
/// the running system is started instead. Then restart (see
/// `process::desktop::reboot`).
pub fn arm(tries: u32) -> Result<String> {
    let Some((slot, version)) = status().ready else {
        return Err("No system update is ready to install.".into());
    };
    authorize(&format!("restart into WaveOS {version}"))?;
    let previous = booted_slot();
    if crate::prefs::get("system_bootloader").as_deref() == Some("legacy") {
        // This bootloader can't start updates: put the update's own in place first.
        let new = fs::read(&format!("{}/BOOTX64.EFI", slot_dir(&slot))).map_err(io)?;
        let tmp = "/Boot/EFI/BOOT/BOOTX64.NEW";
        fs::write(tmp, &new).map_err(io)?;
        fs::try_sync_path(tmp).map_err(io)?;
        if fs::rename(tmp, "/Boot/EFI/BOOT/BOOTX64.EFI").is_err() {
            fs::remove("/Boot/EFI/BOOT/BOOTX64.EFI").map_err(io)?;
            fs::rename(tmp, "/Boot/EFI/BOOT/BOOTX64.EFI").map_err(io)?;
        }
    }
    let tries = tries.clamp(1, 9).to_string();
    edit_boot_conf(&[
        ("system", Some(&slot)),
        ("system_tries", Some(&tries)),
        ("system_previous", Some(&previous)),
        ("system_failed", None),
    ])?;
    let _ = fs::remove(&format!("{SLOTS}/ready"));
    fs::try_sync().map_err(io)?;
    Ok(version)
}

/// Arms the ready update and restarts the computer.
pub fn restart_to_update() -> Result<()> {
    arm(TRIES)?;
    crate::process::desktop::reboot();
    Ok(())
}

/// Throws away an installed update that hasn't been started.
pub fn discard_ready() -> Result<()> {
    if let Some((slot, _)) = status().ready {
        authorize("remove a system update")?;
        let _ = fs::remove(&format!("{SLOTS}/ready"));
        fs::remove_all(&slot_dir(&slot)).map_err(io)?;
    }
    Ok(())
}

/// Forgets that an update failed to start (after telling the person).
pub fn dismiss_failure() -> Result<()> {
    if boot_option("system_failed").is_some() {
        authorize("change how this computer starts up")?;
        edit_boot_conf(&[("system_failed", None)])?;
    }
    Ok(())
}

/// "12.3 MB"
pub fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{}.{} MB", b >> 20, ((b % (1 << 20)) * 10) >> 20),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} bytes"),
    }
}
