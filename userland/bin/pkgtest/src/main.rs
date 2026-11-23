//! The package manager's end-to-end test (run by `cargo xtask test`):
//! installs, updates and removes apps from the host's test repository.
//!
//! `pkgtest BASE install` then `pkgtest BASE remove`; BASE serves the
//! signed test repository at `repo/` and an altered copy at `badrepo/`.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::{format, string::String, vec::Vec};
use corekit::apps::{self, repo, Options, Scope};
use corekit::{fs, println, users};

corekit::entry!(main);

const GALLERY: &str = "dev.example.gallery";
const DOOM: &str = "org.waveos.doom";

fn main(args: corekit::Args) -> i32 {
    let (Some(base), Some(phase)) = (args.get(1), args.get(2)) else {
        println!("usage: pkgtest BASE-URL install|remove");
        return 2;
    };
    let result = match phase.as_str() {
        "install" => install(base),
        "remove" => remove(base),
        _ => Err(String::from("unknown phase")),
    };
    match result {
        Ok(n) => {
            println!("PKGTEST {phase}: all {n} checks passed");
            0
        }
        Err(e) => {
            println!("PKGTEST {phase} FAILED: {e}");
            1
        }
    }
}

fn check(n: &mut u32, ok: bool, what: &str) -> Result<(), String> {
    if !ok {
        return Err(String::from(what));
    }
    *n += 1;
    println!("pkgtest: ok {what}");
    Ok(())
}

fn install(base: &str) -> Result<u32, String> {
    let mut n = 0;
    let source = format!("{base}repo/");
    repo::add_source(&source)?;
    check(&mut n, repo::sources().contains(&source), "repository added")?;

    let bad = repo::fetch_index(&format!("{base}badrepo/"));
    check(&mut n, bad.as_ref().is_err_and(|e| e.contains("altered")), "an index altered after signing is refused")?;

    let found = repo::search("gallery")?;
    check(
        &mut n,
        found.iter().any(|(_, e)| e.id == GALLERY && e.version == "1.1.0"),
        "search finds the newest Gallery",
    )?;

    let a = repo::install(GALLERY, Some("1.0.0"), &Options::default(), |_, _| {})?;
    check(&mut n, a.manifest.version == "1.0.0" && a.scope == Scope::System, "installs Gallery 1.0.0 for everyone")?;
    check(&mut n, !a.receipt.publisher.is_empty() && a.receipt.source.starts_with(&source), "receipt names publisher")?;
    check(&mut n, apps::verify(GALLERY)?.is_empty(), "installed files match their hashes")?;
    let owner = users::owner(&a.executable).map_err(|e| format!("{e}"))?;
    check(&mut n, owner.uid == 0 && owner.mode & 0o777 == 0o755, "program is the system's and runnable")?;
    let probe = format!("{}/probe.txt", apps::data_dir(GALLERY));
    fs::write(&probe, b"keep me").map_err(|e| format!("{e}"))?;

    // An unsigned package needs explicit consent; a forged one never installs.
    let bytes = fs::read(&format!("{}/bin/app", a.directory)).map_err(|e| format!("{e}"))?;
    let mut m = a.manifest.clone();
    m.id = String::from("dev.example.unsigned");
    m.icon = String::new();
    let unsigned = gina::pack(&m, &[("bin/app", &bytes)]).map_err(String::from)?;
    let refused = apps::install_with(&unsigned, &Options::default(), |_, _| {});
    check(&mut n, refused.is_err_and(|e| e.contains("not signed")), "unsigned package refused without consent")?;
    apps::install_with(&unsigned, &Options { allow_untrusted: true, ..Options::default() }, |_, _| {})?;
    apps::uninstall("dev.example.unsigned", true)?;
    check(&mut n, apps::installed("dev.example.unsigned").is_err(), "unsigned package installs only with consent")?;
    let sign = |msg: &[u8]| {
        let mut s = gina::sign::sign(&[1; 32], msg);
        s.signature[0] ^= 1;
        s
    };
    let forged = gina::pack_with(&m, &[("bin/app", &bytes)], Some(&sign)).map_err(String::from)?;
    check(&mut n, apps::review(&forged).is_err(), "a package with a bad signature is refused")?;

    // A stale staging folder from an interrupted install is cleared, and
    // never becomes live.
    let root = format!("/Applications/{GALLERY}");
    fs::mkdir(&format!("{root}/.pending-999999")).map_err(|e| format!("{e}"))?;
    fs::write(&format!("{root}/.pending-999999/junk"), b"half").map_err(|e| format!("{e}"))?;
    check(&mut n, apps::installed(GALLERY)?.manifest.version == "1.0.0", "leftovers don't change the live version")?;

    let ups = repo::updates()?;
    check(&mut n, ups.iter().any(|(a, _, e)| a.manifest.id == GALLERY && e.version == "1.1.0"), "update found")?;
    let b = repo::install(GALLERY, None, &Options::default(), |_, _| {})?;
    check(&mut n, b.manifest.version == "1.1.0" && b.directory != a.directory, "updated to 1.1.0")?;
    check(&mut n, fs::read(&probe).ok().as_deref() == Some(&b"keep me"[..]), "update keeps the app's data")?;
    let left: Vec<String> = fs::read_dir(&root).unwrap_or_default().into_iter().map(|e| e.name).collect();
    check(&mut n, left.len() == 2 && left.iter().any(|e| e == "active"), "old generation and leftovers removed")?;
    let down = repo::install(GALLERY, Some("1.0.0"), &Options::default(), |_, _| {});
    check(&mut n, down.is_err_and(|e| e.contains("newer")), "downgrade refused")?;

    let d = repo::install(DOOM, None, &Options::default(), |_, _| {})?;
    check(&mut n, apps::in_category("game").iter().any(|g| g.manifest.id == DOOM), "Doom listed as a game")?;
    check(&mut n, d.icon().is_some_and(|i| fs::exists(&i)), "Doom has its icon")?;
    apps::set_pinned(DOOM, true)?;
    check(&mut n, apps::pinned().iter().any(|p| p == DOOM), "Doom pinned to the dock")?;

    let reg = apps::registry_path(Scope::System);
    let text = fs::read_to_string(&reg).map_err(|e| format!("{reg}: {e}"))?;
    check(
        &mut n,
        text.lines().any(|l| l.starts_with(&format!("app\t{DOOM}\t1.0.0\tDoom\tgame\t")))
            && text.lines().any(|l| l.starts_with(&format!("app\t{GALLERY}\t1.1.0\t"))),
        "registry lists both, with categories",
    )?;
    let o = users::owner(&reg).map_err(|e| format!("{e}"))?;
    check(&mut n, o.uid == 0 && o.mode & 0o022 == 0, "registry writable only by the system")?;
    Ok(n)
}

fn remove(base: &str) -> Result<u32, String> {
    let mut n = 0;
    apps::uninstall(GALLERY, true)?;
    check(&mut n, apps::installed(GALLERY).is_err(), "Gallery removed")?;
    check(&mut n, !fs::exists(&apps::data_dir(GALLERY)), "and its data, as asked")?;
    apps::uninstall(DOOM, false)?;
    check(&mut n, apps::installed(DOOM).is_err() && !fs::exists(&format!("/Applications/{DOOM}")), "Doom removed")?;
    check(&mut n, fs::exists(&apps::data_dir(DOOM)), "Doom's data kept")?;
    let _ = fs::remove_all(&apps::data_dir(DOOM));
    check(&mut n, !apps::pinned().iter().any(|p| p == DOOM), "Doom unpinned")?;
    let text = fs::read_to_string(&apps::registry_path(Scope::System)).unwrap_or_default();
    check(&mut n, !text.contains(GALLERY) && !text.contains(DOOM), "registry updated")?;
    repo::remove_source(&format!("{base}repo/"))?;
    check(&mut n, repo::sources().is_empty(), "repository removed")?;
    Ok(n)
}
