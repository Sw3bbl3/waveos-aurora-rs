//! Repositories for the Store, and system updates, on the host.
//!
//! `cargo xtask repo FOLDER --sign KEY --out DIR` turns a folder of
//! packages into a signed repository (see `gina::repo`):
//!
//! ```text
//! FOLDER/*.gina                    the packages (any number of versions)
//! FOLDER/<app id>/listing.txt      optional: featured=1, about=…, notes.<version>=…
//!                                  (one line each; \n for a new line)
//! FOLDER/<app id>/screenshots/*.png  optional, in name order (up to 8)
//! ```
//!
//! It writes `catalog.txt`/`catalog.sig` (format 2, with the catalogue
//! `version` one higher than what `DIR` held before and an `expires` time),
//! the format 1 `index.txt`/`index.sig` for older systems, `packages/` and
//! `assets/` (icons from the packages, screenshots, named by hash). Trust
//! roots (`root/<n>.txt`) and system updates (`system/`) in `DIR` are kept.
//!
//! `cargo xtask system-update VERSION --out DIR` adds a system update built
//! from this checkout's EFI partition (kernel, bootloader, system image with
//! `version.txt` saying VERSION), signed with the build's system update key
//! (`target/keys/waveos-system.key`, trusted through
//! `/System/Keys/Updates/waveos-system.pub`), as content-addressed objects
//! under `system/objects/` that later updates share.
//!
//! One folder serves both architectures: `--arch` (or the build's) says
//! which half is written. x86_64 has the names from before the port
//! (`catalog.txt`, `index.txt`, `packages/`, `assets/`, `system/<version>/`),
//! aarch64 its own (`catalog-aarch64.txt`, `packages-aarch64/`,
//! `assets-aarch64/`, `system-aarch64/<version>/`); the content-addressed
//! objects of system updates (`system/objects/`) are shared, and so are the
//! trust roots. Each architecture's catalogue lists only its own packages
//! and updates, and names its architecture (see `gina::repo`).
use super::arch::Arch;
use super::package::{self, Key};
use super::{root, Program};
use gina::repo::{Asset, Entry, Index, Role, Root, SystemRelease};
use gina::system::{self, Item, Part, Update};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// The build's system update key.
pub const SYSTEM_KEY: &str = "target/keys/waveos-system.key";
pub const SYSTEM_PUBLISHER: &str = "WaveOS System Updates (this build)";

/// Store details for one app (from `FOLDER/<id>/`).
#[derive(Clone, Default)]
pub struct Listing {
    pub featured: bool,
    pub about: String,
    /// Release notes by version.
    pub notes: Vec<(String, String)>,
    pub screenshots: Vec<Vec<u8>>,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Reads `dir/listing.txt` and `dir/screenshots/*.png` (both optional).
pub fn read_listing(dir: &Path) -> Listing {
    let mut l = Listing::default();
    for line in fs::read_to_string(dir.join("listing.txt")).unwrap_or_default().lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = gina::repo::unescape(v.trim(), 4000).unwrap_or_default();
        match k.trim() {
            "featured" => l.featured = v == "1",
            "about" => l.about = v,
            k => {
                if let Some(version) = k.strip_prefix("notes.") {
                    l.notes.push((version.into(), v));
                }
            }
        }
    }
    let mut shots: Vec<PathBuf> = fs::read_dir(dir.join("screenshots"))
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "png")).collect())
        .unwrap_or_default();
    shots.sort();
    l.screenshots = shots.iter().take(gina::repo::MAX_SCREENSHOTS).filter_map(|p| fs::read(p).ok()).collect();
    l
}

/// The packages and listings in a repository source folder.
pub fn read_folder(folder: &Path) -> Result<Vec<(Vec<u8>, Listing)>, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "gina"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let bytes = fs::read(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let id = gina::Package::parse(&bytes).map_err(|e| format!("{}: {e}", f.display()))?.manifest.id;
        out.push((bytes, read_listing(&folder.join(id))));
    }
    Ok(out)
}

/// How to write a repository.
pub struct Spec<'a> {
    pub name: &'a str,
    /// Signs the catalogue and the format 1 index.
    pub key: &'a Key,
    /// Extra catalogue signers (a rotation in progress).
    pub cosigners: Vec<&'a Key>,
    pub expires_days: u64,
    pub mirrors: Vec<String>,
    /// The catalogue version; by default one more than the folder held.
    pub version: Option<u64>,
    /// Seconds since 1970 the catalogue was made (default: now).
    pub generated: Option<u64>,
    pub revoked: Vec<(String, String)>,
    /// Whose half of the repository this is (default: the build's).
    pub arch: Arch,
}

/// Where an architecture's files go in a repository folder.
fn sub(arch: Arch, name: &str) -> String {
    format!("{name}{}", arch.suffix())
}

impl<'a> Spec<'a> {
    pub fn new(name: &'a str, key: &'a Key) -> Self {
        Spec {
            name,
            key,
            cosigners: Vec::new(),
            expires_days: 30,
            mirrors: Vec::new(),
            version: None,
            generated: None,
            revoked: Vec::new(),
            arch: crate::arch::current(),
        }
    }
}

fn write_asset(dir: &Path, arch: Arch, bytes: &[u8]) -> Result<Asset, String> {
    let sha: [u8; 32] = Sha256::digest(bytes).into();
    let path = format!("{}/{}.png", sub(arch, "assets"), gina::hex(&sha));
    fs::write(dir.join(&path), bytes).map_err(|e| e.to_string())?;
    Ok(Asset { path, sha256: sha, size: bytes.len() as u64 })
}

fn sign_file(path: &Path, message: &[u8], keys: &[&Key]) -> Result<(), String> {
    let text: String = keys.iter().map(|k| gina::sign::sign(&k.0, message).encode()).collect();
    fs::write(path, text).map_err(|e| e.to_string())
}

/// The catalogue a repository folder holds now for `arch`, if any.
pub fn current_catalog_for(dir: &Path, arch: Arch) -> Option<Index> {
    Index::parse(&fs::read(dir.join(gina::repo::catalog_files(arch.name()).0)).ok()?).ok()
}

/// Writes (or rewrites) the repository in `dir`: packages, pictures, the
/// signed catalogue and format 1 index. Keeps `root/` and `system/`, and
/// lists every system update found in `system/*/update.txt`.
pub fn write(dir: &Path, spec: &Spec, apps: &[(Vec<u8>, Listing)]) -> Result<Index, String> {
    let arch = spec.arch;
    let previous = current_catalog_for(dir, arch).map(|c| c.version).unwrap_or(0);
    for name in ["packages", "assets"] {
        let _ = fs::remove_dir_all(dir.join(sub(arch, name)));
        fs::create_dir_all(dir.join(sub(arch, name))).map_err(|e| e.to_string())?;
    }
    let generated = spec.generated.unwrap_or_else(now);
    let mut index = Index {
        format: 2,
        name: spec.name.into(),
        generated,
        version: spec.version.unwrap_or(previous + 1),
        expires: generated + spec.expires_days * 86400,
        mirrors: spec.mirrors.clone(),
        revoked: spec.revoked.clone(),
        system: system_releases(dir, arch)?,
        architecture: if arch == Arch::X86_64 { String::new() } else { arch.name().into() },
        ..Index::default()
    };
    for (bytes, listing) in apps {
        let p = gina::Package::parse(bytes).map_err(str::to_string)?;
        let m = &p.manifest;
        if m.arch() != arch.name() {
            return Err(format!("{} {} is for {}, and this catalogue for {}", m.id, m.version, m.arch(), arch.name()));
        }
        let file = format!("{}-{}.gina", m.id, m.version);
        fs::write(dir.join(sub(arch, "packages")).join(&file), bytes).map_err(|e| e.to_string())?;
        let icon = p
            .files
            .iter()
            .find(|f| !m.icon.is_empty() && f.path == m.icon)
            .map(|f| write_asset(dir, arch, f.bytes))
            .transpose()?;
        let screenshots =
            listing.screenshots.iter().map(|s| write_asset(dir, arch, s)).collect::<Result<Vec<_>, _>>()?;
        index.packages.push(Entry {
            id: m.id.clone(),
            version: m.version.clone(),
            sha256: Sha256::digest(bytes).into(),
            size: bytes.len() as u64,
            path: format!("{}/{file}", sub(arch, "packages")),
            categories: m.categories.clone(),
            name: m.name.clone(),
            author: m.developer.clone(),
            description: m.description.clone(),
            installed_size: p.installed_size,
            permissions: m.permissions.clone(),
            min_os: m.min_os.clone(),
            published: generated,
            icon,
            screenshots,
            about: listing.about.clone(),
            notes: listing.notes.iter().find(|(v, _)| *v == m.version).map(|(_, n)| n.clone()).unwrap_or_default(),
        });
        if listing.featured && !index.featured.contains(&m.id) {
            index.featured.push(m.id.clone());
        }
    }
    let catalog = index.encode_catalog();
    Index::parse(catalog.as_bytes()).map_err(str::to_string)?;
    let mut signers = vec![spec.key];
    signers.extend(spec.cosigners.iter().copied());
    let (catalog_txt, catalog_sig) = gina::repo::catalog_files(arch.name());
    fs::write(dir.join(catalog_txt), &catalog).map_err(|e| e.to_string())?;
    sign_file(&dir.join(catalog_sig), &gina::repo::catalog_message(catalog.as_bytes()), &signers)?;
    // Format 1 is for systems from before format 2, all of them x86.
    if arch != Arch::X86_64 {
        return Ok(index);
    }
    let v1 = index.encode_v1();
    fs::write(dir.join("index.txt"), &v1).map_err(|e| e.to_string())?;
    sign_file(&dir.join("index.sig"), &gina::repo::signed_message(v1.as_bytes()), &[spec.key])?;
    Ok(index)
}

/// Writes trust root `version` to `dir/root/<version>.txt`, signed by `signers`.
pub fn write_root(
    dir: &Path,
    version: u64,
    root_keys: &[&Key],
    catalog_keys: &[&Key],
    signers: &[&Key],
) -> Result<(), String> {
    let public = |k: &Key| gina::sign::public_key(&k.0);
    let mut keys: Vec<(Role, [u8; 32], String)> =
        root_keys.iter().map(|k| (Role::Root, public(k), k.1.clone())).collect();
    keys.extend(catalog_keys.iter().map(|k| (Role::Catalog, public(k), k.1.clone())));
    let text = Root { version, expires: 0, keys }.encode();
    Root::parse(text.as_bytes()).map_err(str::to_string)?;
    fs::create_dir_all(dir.join("root")).map_err(|e| e.to_string())?;
    fs::write(dir.join(format!("root/{version}.txt")), &text).map_err(|e| e.to_string())?;
    sign_file(&dir.join(format!("root/{version}.sig")), &gina::repo::root_message(text.as_bytes()), signers)
}

/// The system updates in `dir/system/*/update.txt` (`system-aarch64/` for aarch64).
fn system_releases(dir: &Path, arch: Arch) -> Result<Vec<SystemRelease>, String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir.join(sub(arch, "system"))) else { return Ok(out) };
    let mut versions: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("update.txt").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    versions.sort_by(|a, b| gina::compare_versions(a, b));
    for v in versions {
        let path = format!("{}/{v}/update.txt", sub(arch, "system"));
        let bytes = fs::read(dir.join(&path)).map_err(|e| e.to_string())?;
        let u = Update::parse(&bytes).map_err(|e| format!("{path}: {e}"))?;
        if u.arch() != arch.name() {
            return Err(format!("{path} is an update for {}", u.arch()));
        }
        out.push(SystemRelease {
            version: u.version,
            path,
            sha256: Sha256::digest(&bytes).into(),
            size: bytes.len() as u64,
            min_from: u.min_from,
            published: u.published,
        });
    }
    Ok(out)
}

/// This checkout's system update key (made on first use).
pub fn system_key() -> Key {
    let path = root().join(SYSTEM_KEY);
    if path.exists() {
        package::load_key(&path).expect("system update key")
    } else {
        package::keygen(&path, SYSTEM_PUBLISHER).expect("make the system update key")
    }
}

/// Its public half, for `/System/Keys/Updates`.
pub fn system_public_key() -> String {
    let (seed, name) = system_key();
    gina::PublicKey { key: gina::sign::public_key(&seed), name }.encode()
}

fn part(bytes: &[u8]) -> Part {
    Part { sha256: Sha256::digest(bytes).into(), size: bytes.len() as u64 }
}

/// The system image with `version.txt` saying `version`.
pub fn retag_image(tar: &[u8], version: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(tar.len());
    for (item, range) in system::layout_of(tar).map_err(str::to_string)? {
        match item {
            Item::Dir(p) => out.extend_from_slice(&system::tar_header(&p, 0, true)),
            Item::File { path, .. } => {
                let old = &tar[range];
                let data: Vec<u8> = if path == "version.txt" {
                    let text = String::from_utf8_lossy(old);
                    let mut lines: Vec<String> = text.lines().map(String::from).collect();
                    if let Some(first) = lines.first_mut() {
                        *first = format!("WaveOS Aurora {version}");
                    }
                    (lines.join("\n") + "\n").into_bytes()
                } else {
                    old.to_vec()
                };
                out.extend_from_slice(&system::tar_header(&path, data.len() as u64, false));
                out.extend_from_slice(&data);
                out.resize(out.len() + system::padding(data.len() as u64), 0);
            }
        }
    }
    out.resize(out.len() + 1024, 0);
    Ok(out)
}

/// A kernel that never starts: its entry point loads an empty interrupt
/// table and faults, so the processor resets (a triple fault). For the
/// test of an update that fails to boot.
pub fn break_kernel(elf: &[u8]) -> Result<Vec<u8>, String> {
    let u16_at = |o: usize| u16::from_le_bytes([elf[o], elf[o + 1]]) as usize;
    let u64_at = |o: usize| u64::from_le_bytes(elf[o..o + 8].try_into().unwrap());
    if elf.len() < 64 || &elf[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    let (entry, phoff, phentsize, phnum) = (u64_at(24), u64_at(32) as usize, u16_at(54), u16_at(56));
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        let (kind, offset, vaddr, filesz) =
            (u32::from_le_bytes(elf[ph..ph + 4].try_into().unwrap()), u64_at(ph + 8), u64_at(ph + 16), u64_at(ph + 32));
        if kind == 1 && (vaddr..vaddr + filesz).contains(&entry) {
            let at = (offset + entry - vaddr) as usize;
            // push 0; push 0; lidt [rsp]; ud2
            const STUB: [u8; 10] = [0x6A, 0x00, 0x6A, 0x00, 0x0F, 0x01, 0x1C, 0x24, 0x0F, 0x0B];
            let mut out = elf.to_vec();
            out[at..at + STUB.len()].copy_from_slice(&STUB);
            return Ok(out);
        }
    }
    Err("the kernel's entry point is not in a loaded segment".into())
}

/// The update description of the system on an EFI partition folder (what
/// a device starts from), unsigned: `\aurora\update.txt`.
pub fn describe_esp(esp: &Path, version: &str) -> Result<Update, String> {
    let kernel = fs::read(esp.join("aurora/kernel.elf")).map_err(|e| e.to_string())?;
    let boot = fs::read(esp.join("EFI/BOOT").join(crate::arch::current().boot_file())).map_err(|e| e.to_string())?;
    let tar = fs::read(esp.join("aurora/system.tar")).map_err(|e| e.to_string())?;
    system::describe(crate::arch::current().name(), version, "", "", part(&kernel), part(&boot), &tar)
        .map_err(str::to_string)
}

/// How to make a system update.
pub struct Build<'a> {
    pub esp: &'a Path,
    pub version: &'a str,
    pub min_from: &'a str,
    pub notes: &'a str,
    /// Ship a kernel that never starts (tests only).
    pub broken: bool,
}

/// Adds a system update to the repository in `repo`: its objects under
/// `system/objects/`, and `system/<version>/update.txt` signed with `key`.
/// Rewrite the catalogue afterwards ([`write`]) so it lists it.
pub fn add_system_update(repo: &Path, b: &Build, key: &Key) -> Result<SystemRelease, String> {
    let read = |p: &str| fs::read(b.esp.join(p)).map_err(|e| format!("{p}: {e}"));
    let mut kernel = read("aurora/kernel.elf")?;
    if b.broken {
        kernel = break_kernel(&kernel)?;
    }
    let boot = read(&format!("EFI/BOOT/{}", crate::arch::current().boot_file()))?;
    let tar = retag_image(&read("aurora/system.tar")?, b.version)?;
    let arch = crate::arch::current();
    let mut u = system::describe(arch.name(), b.version, b.min_from, b.notes, part(&kernel), part(&boot), &tar)
        .map_err(str::to_string)?;
    u.published = now();
    let objects = repo.join(system::OBJECTS);
    fs::create_dir_all(&objects).map_err(|e| e.to_string())?;
    let put = |data: &[u8]| -> Result<(), String> {
        let path = repo.join(system::object_path(&Sha256::digest(data).into()));
        if !path.exists() {
            fs::write(path, data).map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    put(&kernel)?;
    put(&boot)?;
    for (item, range) in system::layout_of(&tar).map_err(str::to_string)? {
        if let Item::File { size, .. } = item {
            if size > 0 {
                put(&tar[range])?;
            }
        }
    }
    let dir = repo.join(format!("{}/{}", sub(arch, "system"), b.version));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = u.encode();
    fs::write(dir.join("update.txt"), &text).map_err(|e| e.to_string())?;
    sign_file(&dir.join("update.sig"), &system::signed_message(text.as_bytes()), &[key])?;
    Ok(SystemRelease {
        version: u.version.clone(),
        path: format!("{}/{}/update.txt", sub(arch, "system"), b.version),
        sha256: Sha256::digest(text.as_bytes()).into(),
        size: text.len() as u64,
        min_from: u.min_from,
        published: u.published,
    })
}

/// A key made once per checkout for the tests (`target/keys/<name>.key`).
fn test_key(name: &str, publisher: &str) -> Key {
    let path = root().join(format!("target/keys/{name}.key"));
    if path.exists() {
        package::load_key(&path).expect("test key")
    } else {
        package::keygen(&path, publisher).expect("make a test key")
    }
}

/// Listings for the sample apps (`assets/store/<id>/`).
fn sample_listing(id: &str) -> Listing {
    read_listing(&root().join("assets/store").join(id))
}

/// The packages every sample repository offers.
fn samples(programs: &[Program], key: &Key, gallery_versions: &[&str]) -> Vec<(Vec<u8>, Listing)> {
    let mut out: Vec<(Vec<u8>, Listing)> = gallery_versions
        .iter()
        .filter_map(|v| package::gallery(programs, v, Some(key)))
        .map(|p| (p, sample_listing("dev.example.gallery")))
        .collect();
    out.extend(package::doom(programs, Some(key)).map(|p| (p, sample_listing("org.waveos.doom"))));
    out
}

/// The demo repository (`target/demo-repo`): the sample apps with their
/// Store pages, and a system update to the next version, for trying the
/// Store and Software Update (`cargo xtask serve-repo`).
pub fn demo_repository(programs: &[Program], esp: &Path, dir: &Path) {
    let key = package::sample_key();
    // This architecture's updates go; the shared objects stay (the other
    // architecture's updates use them too).
    let arch = crate::arch::current();
    for e in fs::read_dir(dir.join(sub(arch, "system"))).into_iter().flatten().flatten() {
        if e.file_name() != "objects" {
            let _ = fs::remove_dir_all(e.path());
        }
    }
    let next = next_version();
    let notes = "A sample system update made by this build, to try Software Update.";
    let build = Build { esp, version: &next, min_from: "0.8", notes, broken: false };
    add_system_update(dir, &build, &system_key()).expect("demo system update");
    write(dir, &Spec::new("WaveOS Demo", &key), &samples(programs, &key, &["1.0.0", "1.1.0"]))
        .expect("write the demo repository");
}

/// The version after this build's (0.8.0 → 0.8.1).
pub fn next_version() -> String {
    let v = env!("CARGO_PKG_VERSION");
    let (head, last) = v.rsplit_once('.').unwrap_or(("0", v));
    format!("{head}.{}", last.parse::<u32>().unwrap_or(0) + 1)
}

/// The version after that (0.8.0 → 0.8.2).
pub fn version_after_next() -> String {
    let v = next_version();
    let (head, last) = v.rsplit_once('.').unwrap_or(("0", &v));
    format!("{head}.{}", last.parse::<u32>().unwrap_or(0) + 1)
}

/// The repositories the kernel tests use, under `target/test-repo`:
///
/// - `main/`: the samples, a newer Gallery to update to, a good system
///   update (next version) and one whose kernel never starts (the version
///   after), with trust root 1 (the sample key).
/// - `rotated/`: the same packages; root 2 moves the catalogue to a new key
///   that the device does not trust directly.
/// - `stale/`: an older catalogue version (rollback), `expired/`: one past
///   its expiry time (freeze), both validly signed.
pub fn test_repositories(programs: &[Program], esp: &Path, dir: &Path) {
    let key = package::sample_key();
    let _ = fs::remove_dir_all(dir);
    let main = dir.join("main");
    fs::create_dir_all(&main).unwrap();
    let apps = samples(programs, &key, &["1.0.0", "1.1.0"]);
    let (next, broken) = (next_version(), version_after_next());
    let sys = system_key();
    let good = Build { esp, version: &next, min_from: "0.8", notes: "The test update.", broken: false };
    add_system_update(&main, &good, &sys).expect("test system update");
    let bad = Build { esp, version: &broken, min_from: "0.8", notes: "An update that never starts.", broken: true };
    add_system_update(&main, &bad, &sys).expect("broken test system update");
    write_root(&main, 1, &[&key], &[&key], &[&key]).expect("root 1");
    let mut spec = Spec::new("WaveOS Test Repository", &key);
    spec.version = Some(10);
    spec.mirrors = vec![String::from("http://10.0.2.2:1/unreachable/")];
    write(&main, &spec, &apps).expect("write the test repository");

    let stale = dir.join("stale");
    fs::create_dir_all(&stale).unwrap();
    let mut spec = Spec::new("WaveOS Test Repository", &key);
    spec.version = Some(9);
    write(&stale, &spec, &apps).expect("stale repository");

    let expired = dir.join("expired");
    fs::create_dir_all(&expired).unwrap();
    let mut spec = Spec::new("WaveOS Test Repository", &key);
    spec.version = Some(11);
    spec.generated = Some(now() - 40 * 86400);
    write(&expired, &spec, &apps).expect("expired repository");

    let rotated = dir.join("rotated");
    fs::create_dir_all(&rotated).unwrap();
    let (root2, catalog2) = (test_key("test-root-2", "Test Root 2"), test_key("test-catalog-2", "Test Catalogue 2"));
    write_root(&rotated, 1, &[&key], &[&key], &[&key]).expect("root 1");
    write_root(&rotated, 2, &[&root2], &[&catalog2], &[&key, &root2]).expect("root 2");
    write(&rotated, &Spec::new("WaveOS Rotated Repository", &catalog2), &apps).expect("rotated repository");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_kernel_faults_at_its_entry() {
        let mut elf = vec![0u8; 0x200];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[24..32].copy_from_slice(&0x1010u64.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1u32.to_le_bytes());
        elf[72..80].copy_from_slice(&0x100u64.to_le_bytes());
        elf[80..88].copy_from_slice(&0x1000u64.to_le_bytes());
        elf[96..104].copy_from_slice(&0x100u64.to_le_bytes());
        let b = break_kernel(&elf).unwrap();
        assert_eq!(&b[0x110..0x11A], &[0x6A, 0x00, 0x6A, 0x00, 0x0F, 0x01, 0x1C, 0x24, 0x0F, 0x0B]);
        elf[24..32].copy_from_slice(&0x9000u64.to_le_bytes());
        assert!(break_kernel(&elf).is_err());
    }

    #[test]
    fn retagged_image_rebuilds() {
        let mut t = crate::tar::Tar::new();
        t.file("version.txt", b"WaveOS Aurora 0.8.0\nKernel: Aster\n");
        t.dir("Apps");
        t.file("Apps/A.elf", &[5; 3000]);
        let dir = std::env::temp_dir().join(format!("store-tar-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        t.finish(&dir.join("s.tar")).unwrap();
        let tar = fs::read(dir.join("s.tar")).unwrap();
        let new = retag_image(&tar, "0.8.1").unwrap();
        let layout = system::layout_of(&new).unwrap();
        let (_, r) = layout.iter().find(|(i, _)| i.path() == "version.txt").unwrap();
        assert_eq!(&new[r.clone()], b"WaveOS Aurora 0.8.1\nKernel: Aster\n");
        assert_eq!(layout.len(), 3);
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(next_version().split('.').count(), 3);
    }

    /// A package of `arch` whose executable is a minimal ELF for it.
    fn package_for(arch: Arch) -> Vec<u8> {
        let mut elf = vec![0u8; 121];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        let machine: u16 = if arch == Arch::Aarch64 { 183 } else { 62 };
        elf[18..20].copy_from_slice(&machine.to_le_bytes());
        elf[24..32].copy_from_slice(&0x400000u64.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1u32.to_le_bytes());
        elf[68..72].copy_from_slice(&5u32.to_le_bytes());
        elf[72..80].copy_from_slice(&120u64.to_le_bytes());
        elf[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
        elf[96..104].copy_from_slice(&1u64.to_le_bytes());
        elf[104..112].copy_from_slice(&1u64.to_le_bytes());
        let m = gina::Manifest {
            format: 2,
            architecture: arch.name().into(),
            id: "dev.example.both".into(),
            name: "Both".into(),
            developer: "Ex".into(),
            version: "1.0.0".into(),
            entry: "bin/app".into(),
            ..Default::default()
        };
        gina::pack(&m, &[("bin/app", &elf)]).unwrap()
    }

    /// One folder serves both architectures: each catalogue lists its own
    /// package, says its architecture (x86's in the old words), and
    /// writing one leaves the other as it was.
    #[test]
    fn a_repository_holds_both_architectures() {
        let dir = std::env::temp_dir().join(format!("store-arch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let key = ([9u8; 32], String::from("Test"));
        let spec = |arch| Spec { arch, ..Spec::new("Both", &key) };
        let x86 = package_for(Arch::X86_64);
        let arm = package_for(Arch::Aarch64);
        write(&dir, &spec(Arch::X86_64), &[(x86.clone(), Listing::default())]).unwrap();
        let before = fs::read(dir.join("catalog.txt")).unwrap();
        write(&dir, &spec(Arch::Aarch64), &[(arm.clone(), Listing::default())]).unwrap();
        assert_eq!(fs::read(dir.join("catalog.txt")).unwrap(), before, "the x86 catalogue changed");
        let c = current_catalog_for(&dir, Arch::X86_64).unwrap();
        assert_eq!((c.arch(), c.packages[0].path.as_str()), ("x86_64", "packages/dev.example.both-1.0.0.gina"));
        assert!(!String::from_utf8(before).unwrap().contains("\narch\t"));
        assert_eq!(fs::read(dir.join(&c.packages[0].path)).unwrap(), x86);
        let a = current_catalog_for(&dir, Arch::Aarch64).unwrap();
        assert_eq!(
            (a.arch(), a.packages[0].path.as_str()),
            ("aarch64", "packages-aarch64/dev.example.both-1.0.0.gina")
        );
        assert_eq!(fs::read(dir.join(&a.packages[0].path)).unwrap(), arm);
        assert!(dir.join("catalog-aarch64.sig").exists() && dir.join("index.txt").exists());
        // A package for the other architecture is refused.
        assert!(write(&dir, &spec(Arch::Aarch64), &[(x86, Listing::default())]).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
