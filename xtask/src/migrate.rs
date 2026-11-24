//! `cargo xtask migrate-disk`: a new disk for another architecture with the
//! files of an existing one.
//!
//! The home volume (AuroraFS) is the same on x86_64 and aarch64, so
//! everything a person keeps moves as it is: their files, settings,
//! accounts and passwords (`/Security`), apps' data
//! (`~/Library/Containers`), with owners, permissions and times. What was
//! built for the old processor stays behind:
//!
//! - installed apps (`/Applications`, `~/Library/Applications`) whose
//!   packages are for the other architecture: their folders are left out
//!   and each is listed in that root's `.reinstall`
//!   (`gina-reinstall 1`, then `app<TAB>id<TAB>version<TAB>name<TAB>
//!   architecture<TAB>source` per app), which `pkg` shows and which an
//!   install of the same app clears; their data is kept for when they
//!   come back. Apps already for the new architecture move with the rest.
//! - system-update slots (`/.system`): a slot holds one architecture's
//!   kernel and programs. The new disk starts from its own build.
//! - the Linux packages under `/Linux` (Debian's, for the old processor);
//!   the Linux home folders (`/Linux/home`, `/Linux/root`, `/Linux/srv`)
//!   move.
//!
//! The source disk is never written: it is copied first (an instant clone
//! on APFS), and only the copy is mounted (mounting replays the journal,
//! which writes) and deleted afterwards. The new disk is checked with fsck.

use crate::arch::Arch;
use crate::image::{self, Home};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Folders of `/Linux` that hold people's files rather than packages.
const LINUX_KEPT: &[&str] = &["home", "root", "srv"];

/// What a migration did.
#[derive(Debug, Default)]
pub struct Report {
    /// (root, id, version, name, architecture) of apps left to reinstall.
    pub reinstall: Vec<(String, String, String, String, String)>,
    /// Apps that moved (already for the new architecture): (root, id).
    pub moved_apps: Vec<(String, String)>,
    /// Top-level things left out, for the summary.
    pub left_out: Vec<String>,
}

/// An installed app found on the source volume.
struct App {
    root: String,
    id: String,
    version: String,
    name: String,
    arch: String,
    source: String,
}

/// The app roots on a volume: everyone's, the first account's, and each
/// other account's (`/Users/<name>/Library/Applications`).
fn app_roots(vol: &mut Home) -> Vec<String> {
    let mut roots = vec![String::from("/Applications"), String::from("/Library/Applications")];
    for user in image::list(vol, "/Users") {
        roots.push(format!("/Users/{user}/Library/Applications"));
    }
    roots.retain(|r| vol.resolve(r).is_ok());
    roots
}

fn apps(vol: &mut Home) -> Vec<App> {
    let mut out = Vec::new();
    for root in app_roots(vol) {
        for id in image::list(vol, &root) {
            if id.starts_with('.') {
                continue;
            }
            let dir = format!("{root}/{id}");
            let active = String::from_utf8_lossy(&image::home_file(vol, &format!("{dir}/active"))).trim().to_string();
            if active.is_empty() || active.contains('/') {
                continue;
            }
            let text =
                String::from_utf8_lossy(&image::home_file(vol, &format!("{dir}/{active}/manifest.toml"))).into_owned();
            let Ok(m) = gina::Manifest::parse(&text) else { continue };
            let receipt =
                String::from_utf8_lossy(&image::home_file(vol, &format!("{dir}/{active}/META/receipt"))).into_owned();
            let source = receipt.lines().find_map(|l| l.strip_prefix("source=")).unwrap_or_default().to_string();
            out.push(App {
                root: root.clone(),
                id,
                version: m.version.clone(),
                name: m.name.clone(),
                arch: m.arch().to_string(),
                source,
            });
        }
    }
    out
}

/// Builds `out` for `arch` with the EFI partition folder `esp` around a copy
/// of the home volume of `src`. `src` is only read.
pub fn migrate(src: &Path, esp: &Path, out: &Path, arch: Arch) -> io::Result<Report> {
    if same_file(src, out) {
        return Err(io::Error::other("the new disk must be another file than the one it is made from"));
    }
    if out.exists() {
        return Err(io::Error::other(format!(
            "{} exists already; move it away first (it may hold files of its own)",
            out.display()
        )));
    }
    match image::disk_arch(src)? {
        Some(a) if a == arch.name() => {
            return Err(io::Error::other(format!(
                "{} is a disk for {a} already: there is nothing to migrate",
                src.display()
            )))
        }
        _ => {}
    }
    // A scratch copy beside the new disk: mounting replays the journal.
    let copy = scratch(out, "source");
    let _ = fs::remove_file(&copy);
    fs::copy(src, &copy)?;
    let size = fs::metadata(src)?.len().max(256 * 1024 * 1024);
    let building = scratch(out, "new");
    let _ = fs::remove_file(&building);
    let mut report = Report::default();
    let result = (|| {
        // What to leave out is known only once the source is open; the
        // skip list is filled from a first look at a second scratch view.
        let foreign = {
            let mut file = fs::OpenOptions::new().read(true).write(true).open(&copy)?;
            let mut vol = image::open_home(&mut file)?;
            apps(&mut vol)
        };
        let skip_apps: Vec<String> =
            foreign.iter().filter(|a| a.arch != arch.name()).map(|a| format!("{}/{}", a.root, a.id)).collect();
        let registries: Vec<String> = foreign.iter().map(|a| format!("{}/.registry", a.root)).collect();
        let skip = |path: &str| -> bool {
            if path == "/.system" || skip_apps.iter().any(|p| p == path) || registries.iter().any(|p| p == path) {
                return true;
            }
            // `/Linux/<x>`: packages, unless it is a home folder.
            if let Some(rest) = path.strip_prefix("/Linux/") {
                return !rest.contains('/') && !LINUX_KEPT.contains(&rest);
            }
            false
        };
        image::new_disk_around(&copy, esp, &building, size, &skip, &mut |from, to| {
            let err = |e: aurorafs::Error| io::Error::other(format!("AuroraFS: {e:?}"));
            let mut roots: Vec<&str> = foreign.iter().map(|a| a.root.as_str()).collect();
            roots.dedup();
            for root in roots {
                let left: Vec<&App> = foreign.iter().filter(|a| a.root == root && a.arch != arch.name()).collect();
                // The registry lists what is installed: the apps that moved.
                let registry = image::read_with_owner(from, &format!("{root}/.registry"));
                let owner = registry.as_ref().and_then(|(_, o)| *o);
                if let Some((text, _)) = &registry {
                    let kept: String = String::from_utf8_lossy(text)
                        .lines()
                        .filter(|l| {
                            !l.starts_with("app\t") || !left.iter().any(|a| l.split('\t').nth(1) == Some(&a.id))
                        })
                        .map(|l| format!("{l}\n"))
                        .collect();
                    image::write_with_owner(to, &format!("{root}/.registry"), kept.as_bytes(), owner).map_err(err)?;
                }
                if left.is_empty() {
                    continue;
                }
                let mut list = String::from("gina-reinstall 1\n");
                for a in &left {
                    list += &format!("app\t{}\t{}\t{}\t{}\t{}\n", a.id, a.version, a.name, a.arch, a.source);
                }
                // Owned as the registry is: changed by whoever may install there.
                image::write_with_owner(to, &format!("{root}/.reinstall"), list.as_bytes(), owner).map_err(err)?;
            }
            Ok(())
        })?;
        for a in &foreign {
            if a.arch == arch.name() {
                report.moved_apps.push((a.root.clone(), a.id.clone()));
            } else {
                report.reinstall.push((
                    a.root.clone(),
                    a.id.clone(),
                    a.version.clone(),
                    a.name.clone(),
                    a.arch.clone(),
                ));
            }
        }
        {
            let mut file = fs::OpenOptions::new().read(true).write(true).open(&copy)?;
            let mut vol = image::open_home(&mut file)?;
            if vol.resolve("/.system").is_ok() {
                report.left_out.push(String::from("/.system (system-update slots)"));
            }
            for name in image::list(&mut vol, "/Linux") {
                if !LINUX_KEPT.contains(&name.as_str()) {
                    report.left_out.push(format!("/Linux/{name} (Linux packages)"));
                }
            }
        }
        fs::rename(&building, out)
    })();
    let _ = fs::remove_file(&copy);
    if result.is_err() {
        let _ = fs::remove_file(&building);
    }
    result.map(|()| report)
}

fn scratch(out: &Path, what: &str) -> PathBuf {
    out.with_extension(format!("migrating-{what}.img"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn esp(dir: &Path, name: &str, arch: Arch) -> PathBuf {
        let esp = dir.join(name);
        fs::create_dir_all(esp.join("aurora")).unwrap();
        fs::create_dir_all(esp.join("EFI/BOOT")).unwrap();
        fs::write(esp.join("aurora/kernel.elf"), name).unwrap();
        fs::write(esp.join("EFI/BOOT").join(arch.boot_file()), b"MZ loader").unwrap();
        fs::write(esp.join(image::ARCH_FILE), format!("{}\n", arch.name())).unwrap();
        esp
    }

    fn manifest(id: &str, arch: &str) -> String {
        gina::Manifest {
            format: 2,
            architecture: arch.into(),
            id: id.into(),
            name: format!("App {id}"),
            developer: "Example".into(),
            version: "1.2.0".into(),
            entry: "bin/app".into(),
            ..Default::default()
        }
        .encode()
    }

    /// An x86 disk like one that has been used: files with an owner, an
    /// account database, settings, an installed x86 app for everyone and
    /// an aarch64 one for the first account, their data, a system-update
    /// slot and a Linux root with a home folder. Migrated to aarch64: the
    /// person's things are all there, byte for byte, with owners and
    /// times; the x86 app is listed to reinstall; the slot and the Linux
    /// packages are not; the source is unchanged; fsck is clean.
    #[test]
    fn an_x86_disk_moves_to_aarch64() {
        let dir = std::env::temp_dir().join(format!("wv-migrate-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let x86 = esp(&dir, "esp-x86", Arch::X86_64);
        let arm = esp(&dir, "esp-arm", Arch::Aarch64);
        let home = dir.join("home");
        fs::create_dir_all(home.join("Documents")).unwrap();
        fs::write(home.join("Documents/letter.txt"), b"keep me").unwrap();
        fs::create_dir_all(home.join("Settings")).unwrap();
        fs::write(home.join("Settings/aurora.conf"), b"theme=dark\nutc_offset=-300\n").unwrap();
        let src = dir.join("x86.img");
        image::create_with(&x86, &home, None, &src, 160 * 1024 * 1024).unwrap();
        let owner = aurorafs::Owner { uid: 1001, gid: 1001, mode: 0o600 };
        {
            let mut file = fs::OpenOptions::new().read(true).write(true).open(&src).unwrap();
            let mut vol = image::open_home(&mut file).unwrap();
            vol.enable_owners(1000, 1000).unwrap();
            vol.mkdir_all("/Security").unwrap();
            vol.write_file("/Security/users.db", b"me:1000:hash\nbob:1001:hash\n").unwrap();
            vol.mkdir_all("/Users/bob/Documents").unwrap();
            let ino = vol.write_file("/Users/bob/Documents/secret.txt", b"bob's").unwrap();
            vol.set_owner(ino, owner).unwrap();
            for (root, id, arch) in [
                ("/Applications", "dev.example.x86app", "x86_64"),
                ("/Library/Applications", "dev.example.armapp", "aarch64"),
            ] {
                let generation = format!("{root}/{id}/1.2.0-abcd");
                vol.mkdir_all(&format!("{generation}/bin")).unwrap();
                vol.mkdir_all(&format!("{generation}/META")).unwrap();
                vol.write_file(&format!("{generation}/manifest.toml"), manifest(id, arch).as_bytes()).unwrap();
                vol.write_file(&format!("{generation}/bin/app"), b"\x7fELF program").unwrap();
                vol.write_file(
                    &format!("{generation}/META/receipt"),
                    b"installed_at=1\nsource=https://example.org/repo/\n",
                )
                .unwrap();
                vol.write_file(&format!("{root}/{id}/active"), b"1.2.0-abcd\n").unwrap();
                vol.write_file(
                    &format!("{root}/.registry"),
                    format!("gina-registry 1\napp\t{id}\t1.2.0\tApp\n").as_bytes(),
                )
                .unwrap();
            }
            vol.mkdir_all("/Library/Containers/dev.example.x86app").unwrap();
            vol.write_file("/Library/Containers/dev.example.x86app/save.dat", b"level 7").unwrap();
            vol.mkdir_all("/.system/a").unwrap();
            vol.write_file("/.system/a/update.txt", b"gina-system-update 1").unwrap();
            vol.mkdir_all("/Linux/usr/bin").unwrap();
            vol.write_file("/Linux/usr/bin/bash", b"x86 bash").unwrap();
            vol.mkdir_all("/Linux/home/me").unwrap();
            vol.write_file("/Linux/home/me/.bashrc", b"PS1='$ '").unwrap();
            vol.commit().unwrap();
        }
        let before = Sha256::digest(fs::read(&src).unwrap());
        let out = dir.join("arm.img");
        let report = migrate(&src, &arm, &out, Arch::Aarch64).unwrap();
        assert_eq!(Sha256::digest(fs::read(&src).unwrap()), before, "the source disk changed");
        assert!(!dir.join("arm.migrating-source.img").exists() && !dir.join("arm.migrating-new.img").exists());
        assert_eq!(image::disk_arch(&out).unwrap().as_deref(), Some("aarch64"));
        assert_eq!(report.reinstall.len(), 1);
        assert_eq!(report.reinstall[0].1, "dev.example.x86app");
        assert_eq!(report.moved_apps, [(String::from("/Library/Applications"), String::from("dev.example.armapp"))]);
        assert!(report.left_out.iter().any(|l| l.starts_with("/.system")));
        assert!(report.left_out.iter().any(|l| l.starts_with("/Linux/usr")));

        let mut file = fs::OpenOptions::new().read(true).write(true).open(&out).unwrap();
        let mut vol = image::open_home(&mut file).unwrap();
        assert!(vol.fsck().is_ok());
        assert_eq!(image::home_file(&mut vol, "/Documents/letter.txt"), b"keep me");
        assert_eq!(image::home_file(&mut vol, "/Settings/aurora.conf"), b"theme=dark\nutc_offset=-300\n");
        assert_eq!(image::home_file(&mut vol, "/Security/users.db"), b"me:1000:hash\nbob:1001:hash\n");
        let (data, own) = image::read_with_owner(&mut vol, "/Users/bob/Documents/secret.txt").unwrap();
        assert_eq!((data.as_slice(), own), (&b"bob's"[..], Some(owner)));
        assert_eq!(vol.default_owner(), (1000, 1000));
        // The x86 app: gone, listed, its data kept.
        assert!(vol.resolve("/Applications/dev.example.x86app").is_err());
        let list = String::from_utf8(image::home_file(&mut vol, "/Applications/.reinstall")).unwrap();
        assert_eq!(
            list,
            "gina-reinstall 1\napp\tdev.example.x86app\t1.2.0\tApp dev.example.x86app\tx86_64\thttps://example.org/repo/\n"
        );
        assert_eq!(image::home_file(&mut vol, "/Applications/.registry"), b"gina-registry 1\n");
        assert_eq!(image::home_file(&mut vol, "/Library/Containers/dev.example.x86app/save.dat"), b"level 7");
        // The aarch64 app moved as it was.
        assert_eq!(
            image::home_file(&mut vol, "/Library/Applications/dev.example.armapp/1.2.0-abcd/bin/app"),
            b"\x7fELF program"
        );
        assert!(image::home_file(&mut vol, "/Library/Applications/.registry")
            .ends_with(b"dev.example.armapp\t1.2.0\tApp\n"));
        assert!(vol.resolve("/Library/Applications/.reinstall").is_err());
        // No update slots, no x86 Linux packages; the Linux home stays.
        assert!(vol.resolve("/.system").is_err());
        assert!(vol.resolve("/Linux/usr").is_err());
        assert_eq!(image::home_file(&mut vol, "/Linux/home/me/.bashrc"), b"PS1='$ '");
        drop(vol);
        // Never over an existing disk, never onto itself, never the same way.
        assert!(migrate(&src, &arm, &out, Arch::Aarch64).is_err());
        assert!(migrate(&src, &arm, &src, Arch::Aarch64).is_err());
        assert!(migrate(&out, &arm, &dir.join("again.img"), Arch::Aarch64).is_err());
        assert_eq!(Sha256::digest(fs::read(&src).unwrap()), before);
        fs::remove_dir_all(&dir).unwrap();
    }
}
