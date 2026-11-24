//! The package manager's and system updater's end-to-end tests (run by
//! `cargo xtask test`, against the repositories the harness serves; see
//! `repo_response` in xtask):
//!
//! ```text
//! pkgtest BASE install          install, update and remove from a repository
//! pkgtest BASE remove
//! pkgtest BASE store            catalogue signatures, rollback and freeze, key
//!                               rotation, damaged and cut-short downloads, resume,
//!                               pause and cancel, app update and rollback
//! pkgtest BASE slowinstall      an install that waits between files (to be killed)
//! pkgtest BASE recover          after the kill: nothing half-installed
//! pkgtest BASE powercut         an install the harness cuts the power during
//! pkgtest BASE powercut-check   after the cut: the old state or the new, never a mix
//! pkgtest BASE system-stage V   download and install system update V, start it next
//! pkgtest BASE system-broken V  the same with V, whose kernel never starts (1 try)
//! pkgtest BASE system-fallback V  after V failed: back on the previous system
//! ```
#![no_std]
#![no_main]
extern crate alloc;
use alloc::{format, string::String, vec::Vec};
use corekit::apps::download::{Control, Outcome};
use corekit::apps::repo::{self, Progress};
use corekit::apps::{self, Options, Scope};
use corekit::{fs, println, update, users};

corekit::entry!(main);

const GALLERY: &str = "dev.example.gallery";
const DOOM: &str = "org.waveos.doom";
/// Written by `slowinstall` and `powercut` once files are being written.
pub const MARKER: &str = "/pkgtest-installing";

fn main(args: corekit::Args) -> i32 {
    let (Some(base), Some(phase)) = (args.get(1), args.get(2)) else {
        println!("usage: pkgtest BASE-URL PHASE");
        return 2;
    };
    let version = args.get(3).cloned().unwrap_or_default();
    let result = match phase.as_str() {
        "install" => install(base),
        "remove" => remove(base),
        "store" => store(base),
        "slowinstall" => slow_install(base, false),
        "recover" => recover(),
        "powercut" => slow_install(base, true),
        "powercut-check" => powercut_check(),
        "system-stage" => system_stage(base, &version, 3),
        "system-broken" => system_stage(base, &version, 1),
        "system-fallback" => system_fallback(&version),
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
    check(&mut n, bad.as_ref().is_err_and(|e| e.contains("altered")), "a catalogue altered after signing is refused")?;

    let c = repo::catalog(&source)?;
    check(
        &mut n,
        c.index.format == 2 && c.index.version == 10,
        "the repository serves a signed catalogue (version 10)",
    )?;
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
    check(
        &mut n,
        left.len() == 4 && left.iter().any(|e| e == "active") && left.iter().any(|e| e == "previous"),
        "leftovers removed; the previous version kept for rollback",
    )?;
    let down = repo::install(GALLERY, Some("1.0.0"), &Options::default(), |_, _| {});
    check(&mut n, down.is_err_and(|e| e.contains("newer")), "downgrade refused")?;

    // Rolling an update back, and forward again.
    check(&mut n, apps::previous_version(GALLERY).as_deref() == Some("1.0.0"), "the previous version is offered")?;
    let r = apps::rollback(GALLERY)?;
    check(&mut n, r.manifest.version == "1.0.0" && apps::verify(GALLERY)?.is_empty(), "rolled back to 1.0.0")?;
    check(&mut n, fs::read(&probe).ok().as_deref() == Some(&b"keep me"[..]), "rollback keeps the app's data")?;
    let f = apps::rollback(GALLERY)?;
    check(&mut n, f.manifest.version == "1.1.0", "and forward to 1.1.0")?;

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

/// Fetches `path` from the test server.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let u = nebula_web::Url::parse(url).ok_or("bad address")?;
    let r = corekit::web::fetch(&u).map_err(|e| format!("{url}: {e}"))?;
    if r.status != 200 {
        return Err(format!("{url}: {}", r.status));
    }
    Ok(r.body)
}

/// Copies a served repository's catalogue (and roots) into a local folder.
fn copy_catalog(base: &str, from: &str, to: &str, roots: &[u32]) -> Result<(), String> {
    let _ = fs::mkdir(to);
    let _ = fs::mkdir(&format!("{to}root"));
    let (txt, sig) = gina::repo::catalog_files(gina::NATIVE_ARCH);
    for f in [txt, sig] {
        fs::write(&format!("{to}{f}"), &fetch(&format!("{base}{from}/{f}"))?).map_err(|e| format!("{e}"))?;
    }
    for r in roots {
        for ext in ["txt", "sig"] {
            let f = format!("root/{r}.{ext}");
            fs::write(&format!("{to}{f}"), &fetch(&format!("{base}{from}/{f}"))?).map_err(|e| format!("{e}"))?;
        }
    }
    Ok(())
}

fn store(base: &str) -> Result<u32, String> {
    let mut n = 0;
    // Freshness, through a folder repository whose catalogue is swapped:
    // the same address serving version 10, then 9 (a rollback), then an
    // expired 11 (a freeze).
    let local = "/pkgtest-repo/";
    let source = format!("file://{local}");
    let _ = fs::remove_all(local);
    copy_catalog(base, "repo", local, &[])?;
    let c = repo::catalog(&source)?;
    check(&mut n, c.index.version == 10, "a folder repository's catalogue is accepted")?;
    copy_catalog(base, "stale", local, &[])?;
    let e = repo::catalog(&source).err().unwrap_or_default();
    check(&mut n, e.contains("rollback"), "an older catalogue from the same repository is refused (rollback)")?;
    copy_catalog(base, "expired", local, &[])?;
    let e = repo::catalog(&source).err().unwrap_or_default();
    check(&mut n, e.contains("expired"), "an expired catalogue is refused (freeze)")?;
    check(
        &mut n,
        repo::cached(&source).is_some_and(|c| c.index.version == 10),
        "the last good catalogue stays cached",
    )?;
    let _ = fs::remove_all(local);

    // Key rotation: root 2 moves the catalogue to a key this computer
    // doesn't trust directly; without the root it is refused.
    let rotated = repo::catalog(&format!("{base}rotated/"))?;
    check(
        &mut n,
        rotated.index.name == "WaveOS Rotated Repository",
        "a catalogue signed by a rotated key is accepted via the roots",
    )?;
    let local2 = "/pkgtest-rotated/";
    let _ = fs::remove_all(local2);
    copy_catalog(base, "rotated", local2, &[1])?;
    let e = repo::catalog(&format!("file://{local2}")).err().unwrap_or_default();
    check(&mut n, e.contains("trust") || e.contains("altered"), "without the next root, the rotated key is refused")?;
    let _ = fs::remove_all(local2);

    // Downloads: damaged and cut short are refused before anything is
    // unpacked; a dropped connection resumes.
    let opts = Options { scope: Scope::User, ..Options::default() };
    let control = Control::new();
    for (path, what) in [("corrupt", "a damaged download is refused"), ("truncated", "a cut-short download is refused")]
    {
        let c = repo::catalog(&format!("{base}{path}/"))?;
        let e = c.index.latest(DOOM).cloned().ok_or("no Doom")?;
        let r = repo::install_entry(&c, &e, &opts, &control, |_| {});
        let refused = r.as_ref().err().is_some_and(|e| e.contains("damaged")) && apps::installed(DOOM).is_err();
        check(&mut n, refused, &format!("{what} ({:?})", r.as_ref().err()))?;
        check(&mut n, !fs::exists(&format!("{}.part", repo::download_path(&e))), "and its partial file is gone")?;
    }
    let c = repo::catalog(&format!("{base}flaky/"))?;
    let e = c.index.latest(DOOM).cloned().ok_or("no Doom")?;
    let mut ranged = 0u64;
    let r = repo::install_entry(&c, &e, &opts, &control, |p| {
        if let Progress::Downloading(got, _) = p {
            ranged = ranged.max(got);
        }
    })?;
    check(
        &mut n,
        r.is_some_and(|a| a.scope == Scope::User) && apps::verify(DOOM)?.is_empty(),
        "a broken-off download resumes with a range and installs for one account",
    )?;
    apps::uninstall(DOOM, true)?;

    // Pause, resume and cancel a slow download.
    let c = repo::catalog(&format!("{base}slow/"))?;
    let e = c.index.latest(DOOM).cloned().ok_or("no Doom")?;
    let part = format!("{}.part", repo::download_path(&e));
    let control = alloc::sync::Arc::new(Control::new());
    let (c2, e2, ctl) = (c.clone(), e.clone(), control.clone());
    let worker =
        corekit::thread::spawn(move || repo::download_entry(&c2, &e2, &ctl, &mut |_, _| {}).map(|b| b.is_some()))
            .map_err(|e| format!("{e}"))?;
    corekit::time::sleep_ms(600);
    control.pause();
    let paused = worker.join();
    let size = fs::stat(&part).map(|s| s.size).unwrap_or(0);
    check(&mut n, paused == Ok(false) && size > 0 && size < e.size, "a paused download keeps what arrived")?;
    control.resume();
    let done = repo::download_entry(&c, &e, &control, &mut |_, _| {})?;
    check(&mut n, done.is_some_and(|b| b.len() as u64 == e.size), "and resumes to the whole, verified package")?;
    let _ = fs::remove(&repo::download_path(&e));
    let control = alloc::sync::Arc::new(Control::new());
    let (c2, e2, ctl) = (c.clone(), e.clone(), control.clone());
    let worker =
        corekit::thread::spawn(move || repo::download_entry(&c2, &e2, &ctl, &mut |_, _| {}).map(|b| b.is_some()))
            .map_err(|e| format!("{e}"))?;
    corekit::time::sleep_ms(400);
    control.cancel();
    let cancelled = worker.join();
    check(&mut n, cancelled == Ok(false) && !fs::exists(&part), "a cancelled download leaves nothing behind")?;
    Ok(n)
}

/// Installs Gallery 1.0.0 slowly; `cut`: the harness cuts the power.
fn slow_install(base: &str, cut: bool) -> Result<u32, String> {
    let source = format!("{base}repo/");
    if !repo::sources().contains(&source) {
        repo::add_source(&source)?;
    }
    let (c, e) = repo::find(if cut { DOOM } else { GALLERY }, None)?;
    let bytes = repo::download_entry(&c, &e, &Control::new(), &mut |_, _| {})?.ok_or("stopped")?;
    let mut started = false;
    apps::install_with(&bytes, &Options::default(), |i, total| {
        if i == 2 && !started {
            started = true;
            let _ = fs::write(MARKER, b"installing");
            fs::sync();
            if cut {
                println!("harness: power cut (pkgtest installing file {i} of {total})");
                loop {
                    corekit::time::sleep_ms(1000);
                }
            }
        }
        corekit::time::sleep_ms(400);
    })?;
    Ok(0)
}

/// After an install was killed: the app is as before (not installed),
/// its staging folder is cleared, and installing again works.
fn recover() -> Result<u32, String> {
    let mut n = 0;
    check(&mut n, apps::installed(GALLERY).is_err(), "a killed install left nothing live")?;
    let root = format!("/Applications/{GALLERY}");
    let staged = fs::read_dir(&root).unwrap_or_default().iter().any(|e| e.name.starts_with(".pending-"));
    check(&mut n, staged, "the killed install's staging folder is there")?;
    apps::recover();
    let left: Vec<String> = fs::read_dir(&root)
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .filter(|e| e.starts_with(".pending-"))
        .collect();
    if !left.is_empty() {
        let live: Vec<(u32, u32)> = corekit::process::list().iter().map(|p| (p.pid, p.state)).collect();
        println!(
            "pkgtest: still there: {left:?}; processes (pid, state) {live:?}; this boot began at {}",
            corekit::time::boot_file_time()
        );
        for l in &left {
            println!("pkgtest: {l}: {:?}", fs::stat(&format!("{root}/{l}")).map(|s| s.mtime));
        }
    }
    check(&mut n, left.is_empty(), "and recovery clears it")?;
    let a = repo::install(GALLERY, Some("1.0.0"), &Options::default(), |_, _| {})?;
    check(&mut n, a.manifest.version == "1.0.0" && apps::verify(GALLERY)?.is_empty(), "installing again works")?;
    apps::uninstall(GALLERY, true)?;
    let _ = fs::remove(MARKER);
    Ok(n)
}

/// After the power was cut mid-install: Doom is not live (the pointer
/// never moved) or wholly there, and nothing half-written remains.
fn powercut_check() -> Result<u32, String> {
    let mut n = 0;
    check(&mut n, fs::exists(MARKER), "the previous start was cut off mid-install")?;
    match apps::installed(DOOM) {
        Ok(_) => check(&mut n, apps::verify(DOOM)?.is_empty(), "the app is wholly installed")?,
        Err(_) => check(&mut n, true, "the app is not installed (the cut came before it went live)")?,
    }
    let root = format!("/Applications/{DOOM}");
    let names = |r: &str| -> Vec<String> { fs::read_dir(r).unwrap_or_default().into_iter().map(|e| e.name).collect() };
    println!("pkgtest: after the cut, {root} holds {:?}", names(&root));
    apps::recover();
    let left: Vec<String> = names(&root).into_iter().filter(|e| e.starts_with(".pending-")).collect();
    for l in &left {
        println!("pkgtest: {l} left: removing it says {:?}", fs::remove_all(&format!("{root}/{l}")));
    }
    check(&mut n, left.is_empty(), "no staging folder is left after recovery")?;
    if apps::installed(DOOM).is_ok() {
        apps::uninstall(DOOM, true)?;
    } else {
        let _ = fs::remove_all(&root);
    }
    let _ = fs::remove(MARKER);
    let _ = repo::remove_source(&format!("{}repo/", ""));
    Ok(n)
}

/// Downloads and installs system update `version` and arms it.
fn system_stage(base: &str, version: &str, tries: u32) -> Result<u32, String> {
    let mut n = 0;
    let source = format!("{base}repo/");
    for s in repo::sources() {
        if s != source {
            let _ = repo::remove_source(&s);
        }
    }
    if !repo::sources().contains(&source) {
        repo::add_source(&source)?;
    }
    let before = update::status();
    let av = update::check(Some(version))?.ok_or("the update is not offered")?;
    check(&mut n, av.update.version == version, "the system update is found and its signature checked")?;
    let objects = av.missing.len();
    check(
        &mut n,
        (1..=3).contains(&objects) && av.download_size() < av.installed_size() / 4,
        "only the changed files are downloaded",
    )?;
    println!(
        "pkgtest: {objects} objects, {} of {} to download",
        update::human(av.download_size()),
        update::human(av.installed_size())
    );
    let got = update::download(&av, &Control::new(), |_, _| {})?;
    check(&mut n, got == Outcome::Done && av.download_size() == 0, "downloaded and verified")?;
    let slot = update::install(&av, |_, _| {})?;
    let want = if before.slot == "a" { "b" } else { "a" };
    check(&mut n, slot == want, "installed into the slot that isn't running")?;
    let st = update::status();
    check(&mut n, st.ready.as_ref().is_some_and(|(s, v)| s == want && v == version), "the update is ready")?;
    let v = update::arm(tries)?;
    let conf = update::boot_options();
    let opt = |k: &str| conf.iter().find(|(c, _)| c == k).map(|(_, v)| v.clone());
    check(
        &mut n,
        v == version && opt("system").as_deref() == Some(want) && opt("system_tries") == Some(format!("{tries}")),
        "the next start uses it, on trial",
    )?;
    check(
        &mut n,
        opt("system_previous").as_deref() == Some(before.slot.as_str()),
        "with the running system to go back to",
    )?;
    Ok(n)
}

/// After a system that never started: back on the previous one.
fn system_fallback(version: &str) -> Result<u32, String> {
    let mut n = 0;
    let st = update::status();
    check(&mut n, st.failed.as_deref() == Some(version), "the failed version is reported")?;
    check(&mut n, st.version != version && !st.trial, "the previous system is running, not on trial")?;
    update::dismiss_failure()?;
    check(&mut n, update::status().failed.is_none(), "the report can be dismissed")?;
    Ok(n)
}
