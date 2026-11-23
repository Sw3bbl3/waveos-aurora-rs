//! pkg — install, update, remove and inspect apps (GINA packages).
//!
//! ```text
//! pkg install FILE.gina | URL | ID   [--user] [--version V] [--allow-untrusted] [--allow-downgrade]
//! pkg remove ID [--delete-data]
//! pkg list [--category C]          pkg info ID|FILE        pkg verify [ID]
//! pkg update [ID] [--check]        pkg search [TERM]
//! pkg source [list|add URL|remove URL]
//! pkg trust FILE.pub | pkg untrust KEYHEX | pkg keys
//! pkg pin ID | pkg unpin ID        pkg registry
//! ```
#![no_std]
#![no_main]
extern crate alloc;
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use corekit::apps::{self, repo, trust, Options, Review, Scope, Trust};
use corekit::{fs, print, println};

corekit::entry!(main);

type Result<T> = core::result::Result<T, String>;

fn main(args: corekit::Args) -> i32 {
    match run(&args[1.min(args.len())..]) {
        Ok(()) => 0,
        Err(e) => {
            corekit::eprintln!("pkg: {e}");
            1
        }
    }
}

const HELP: &str = "pkg — apps for WaveOS (GINA packages)

  install FILE.gina|URL|ID   install a package file, a download, or an app from the repositories
      --user                 just for you (in ~/Library/Applications), not for everyone
      --version V            a particular version from the repositories
      --allow-untrusted      install even if unsigned or signed by a key you don't trust
      --allow-downgrade      replace a newer installed version
  remove ID [--delete-data]  uninstall (keeps the app's data unless asked)
  list [--category C]        installed apps (C: game, utility, ...)
  info ID|FILE               details, permissions and signature
  verify [ID]                check installed files against their hashes
  update [ID] [--check]      install newer versions from the repositories
  search [TERM]              find apps in the repositories
  source [list|add URL|remove URL]   package repositories
  trust FILE.pub | untrust KEY | keys   publisher keys
  pin ID | unpin ID          keep an app in the dock
  registry                   the installed-apps registry, as programs read it";

fn flag(args: &[String], f: &str) -> bool {
    args.iter().any(|a| a == f)
}

fn value<'a>(args: &'a [String], f: &str) -> Option<&'a str> {
    args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// Arguments that are not flags (or flag values).
fn plain(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if a == "--version" || a == "--category" {
            skip = true;
        } else if !a.starts_with("--") {
            out.push(a.as_str());
        }
    }
    out
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{}.{} MB", b >> 20, ((b % (1 << 20)) * 10) >> 20),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} bytes"),
    }
}

fn show_review(r: &Review) {
    let m = &r.manifest;
    println!("{} {}  ({})", m.name, m.version, m.id);
    println!("  by {}", m.developer);
    if !m.description.is_empty() {
        println!("  {}", m.description);
    }
    println!("  {}", r.trust.describe());
    println!("  size {}", human(r.size));
    if !m.categories.is_empty() {
        println!("  category {}", m.categories.join(", "));
    }
    if !m.extensions.is_empty() {
        println!("  opens .{}", m.extensions.join(", ."));
    }
    if m.permissions.is_empty() {
        println!("  asks for no permissions");
    } else {
        println!("  asks to:");
        for p in &m.permissions {
            println!("    {} {}", if gina::sensitive_permission(p) { "!" } else { "-" }, gina::describe_permission(p));
        }
    }
    if let Some(c) = &r.current {
        println!(
            "  {} version {} (installed for {})",
            if r.is_downgrade() { "replaces newer" } else { "updates" },
            c.manifest.version,
            c.scope.name()
        );
    }
    for b in &r.blockers {
        println!("  cannot install: {b}");
    }
}

fn progress(n: usize, total: usize) {
    // Whole lines only (Terminal has no carriage return).
    if n == 1 {
        println!("  installing {total} files…");
    }
}

fn options(args: &[String], source: &str) -> Options {
    Options {
        scope: if flag(args, "--user") { Scope::User } else { Scope::System },
        allow_untrusted: flag(args, "--allow-untrusted"),
        allow_downgrade: flag(args, "--allow-downgrade"),
        source: source.into(),
    }
}

fn warn_untrusted(trust: &Trust) -> Result<()> {
    match trust {
        Trust::Trusted(_) => Ok(()),
        t => Err(format!(
            "{}. Anyone could have made or altered it. If you trust where it came from, run again with --allow-untrusted.",
            t.describe()
        )),
    }
}

fn download(url: &str) -> Result<Vec<u8>> {
    let u = nebula_web::Url::parse(url).ok_or("bad address")?;
    let r = corekit::web::fetch(&u).map_err(|e| format!("download failed ({e:?})"))?;
    if r.status != 200 {
        return Err(format!("the server answered {} {}", r.status, r.reason));
    }
    if r.body.len() > gina::MAX_PACKAGE {
        return Err("the download is larger than any package may be".into());
    }
    Ok(r.body)
}

fn install(args: &[String]) -> Result<()> {
    let target = *plain(args).get(1).ok_or("install what? A .gina file, an address or an app ID")?;
    let (bytes, source) = if target.starts_with("https://") || target.starts_with("http://") {
        (download(target)?, String::from(target))
    } else if fs::exists(target) {
        let path = fs::resolve(&fs::cwd(), target);
        (fs::read(&path).map_err(|e| format!("{target}: {e}"))?, path)
    } else if gina::valid_id(target) {
        let (src, e) = repo::find(target, value(args, "--version"))?;
        println!("{} {} from {src}", e.name, e.version);
        let bytes = repo::download(&src, &e)?;
        (bytes, format!("{src}{}", e.path))
    } else {
        return Err(format!("{target}: no such file, and not an app ID"));
    };
    let review = apps::review(&bytes)?;
    show_review(&review);
    if let Some(b) = review.blockers.first() {
        return Err(b.clone());
    }
    if !flag(args, "--allow-untrusted") {
        warn_untrusted(&review.trust)?;
    }
    let mut opts = options(args, &source);
    // The person was shown the warning and passed --allow-untrusted, or it is trusted.
    opts.allow_untrusted = true;
    let app = apps::install_with(&bytes, &opts, progress)?;
    println!("Installed {} {} for {}.", app.manifest.name, app.manifest.version, app.scope.name());
    Ok(())
}

fn info(args: &[String]) -> Result<()> {
    let target = *plain(args).get(1).ok_or("info about what?")?;
    if fs::exists(target) && !fs::is_dir(target) {
        let bytes = fs::read(target).map_err(|e| format!("{target}: {e}"))?;
        show_review(&apps::review(&bytes)?);
        return Ok(());
    }
    let a = apps::installed(target)?;
    let m = &a.manifest;
    println!("{} {}  ({})", m.name, m.version, m.id);
    println!("  by {}", m.developer);
    if !m.description.is_empty() {
        println!("  {}", m.description);
    }
    println!("  installed for {} in {}", a.scope.name(), a.directory);
    println!("  data in {}", a.data_dir());
    let signed = if a.receipt.publisher.is_empty() && a.receipt.signer.is_empty() {
        String::from("not signed")
    } else if a.receipt.publisher.is_empty() {
        format!("signed by an untrusted key {}", a.receipt.signer)
    } else {
        format!("signed by {}", a.receipt.publisher)
    };
    println!("  {signed}; size {}", human(a.receipt.size));
    if !a.receipt.source.is_empty() {
        println!("  from {}", a.receipt.source);
    }
    if !m.categories.is_empty() {
        println!("  category {}", m.categories.join(", "));
    }
    for p in &m.permissions {
        println!("  may: {}", gina::describe_permission(p));
    }
    Ok(())
}

fn verify(args: &[String]) -> Result<()> {
    let ids: Vec<String> = match plain(args).get(1) {
        Some(id) => alloc::vec![String::from(*id)],
        None => apps::list().into_iter().map(|a| a.manifest.id).collect(),
    };
    let mut bad = 0;
    for id in ids {
        match apps::verify(&id) {
            Ok(files) if files.is_empty() => println!("{id}: ok"),
            Ok(files) => {
                bad += 1;
                for f in files {
                    println!("{id}: CHANGED OR MISSING {f}");
                }
            }
            Err(e) => println!("{id}: {e}"),
        }
    }
    if bad > 0 {
        Err(format!("{bad} app(s) failed verification; reinstall them"))
    } else {
        Ok(())
    }
}

fn update(args: &[String]) -> Result<()> {
    let only = plain(args).get(1).map(|s| String::from(*s));
    let mut ups = repo::updates()?;
    if let Some(id) = &only {
        ups.retain(|(a, _, _)| &a.manifest.id == id);
    }
    if ups.is_empty() {
        println!("Everything is up to date.");
        return Ok(());
    }
    for (app, _, e) in &ups {
        println!("{}: {} → {}", app.manifest.name, app.manifest.version, e.version);
    }
    if flag(args, "--check") {
        return Ok(());
    }
    for (app, source, e) in ups {
        let bytes = repo::download(&source, &e)?;
        let review = apps::review(&bytes)?;
        if !flag(args, "--allow-untrusted") {
            warn_untrusted(&review.trust)?;
        }
        let opts = Options { scope: app.scope, allow_untrusted: true, ..options(args, &format!("{source}{}", e.path)) };
        let new = apps::install_with(&bytes, &opts, progress)?;
        println!("Updated {} to {}.", new.manifest.name, new.manifest.version);
    }
    Ok(())
}

fn run(args: &[String]) -> Result<()> {
    let p = plain(args);
    match p.first().copied().unwrap_or("help") {
        "install" => install(args),
        "remove" | "uninstall" => {
            let id = *p.get(1).ok_or("remove which app?")?;
            let name = apps::installed(id)?.manifest.name;
            apps::uninstall(id, flag(args, "--delete-data"))?;
            println!(
                "Removed {name}{}.",
                if flag(args, "--delete-data") { " and its data" } else { " (its data is kept)" }
            );
            Ok(())
        }
        "list" => {
            let category = value(args, "--category");
            for a in apps::list() {
                if category.is_some_and(|c| !a.manifest.categories.iter().any(|x| x == c)) {
                    continue;
                }
                println!(
                    "{:<32} {:<10} {:<9} {:<12} {}",
                    a.manifest.id,
                    a.manifest.version,
                    a.scope.name(),
                    a.manifest.categories.first().map(String::as_str).unwrap_or("-"),
                    a.manifest.name
                );
            }
            Ok(())
        }
        "info" => info(args),
        "verify" => verify(args),
        "update" | "upgrade" => update(args),
        "search" => {
            let found = repo::search(p.get(1).copied().unwrap_or(""))?;
            if found.is_empty() {
                println!("Nothing found.");
            }
            for (_, e) in found {
                let mark = if apps::installed(&e.id).is_ok() { "installed" } else { "" };
                println!("{:<32} {:<10} {:<10} {} — {}", e.id, e.version, mark, e.name, e.description);
            }
            Ok(())
        }
        "source" | "sources" => match (p.get(1).copied(), p.get(2).copied()) {
            (None | Some("list"), _) => {
                for s in repo::sources() {
                    println!("{s}");
                }
                Ok(())
            }
            (Some("add"), Some(url)) => repo::add_source(url).map(|_| println!("Added {url}")),
            (Some("remove"), Some(url)) => repo::remove_source(url).map(|_| println!("Removed {url}")),
            _ => Err("pkg source [list|add URL|remove URL]".into()),
        },
        "keys" => {
            for k in trust::keys() {
                println!("{}  {}", gina::fingerprint(&k.key), k.name);
            }
            Ok(())
        }
        "trust" => {
            let file = *p.get(1).ok_or("pkg trust FILE.pub")?;
            let key = trust::parse_key(&fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?)?;
            trust::add_key(&key)?;
            println!("Now trusting {} (key {}).", key.name, gina::fingerprint(&key.key));
            Ok(())
        }
        "untrust" => {
            let key = gina::unhex::<32>(p.get(1).ok_or("pkg untrust KEYHEX")?).ok_or("a key is 64 hex digits")?;
            trust::remove_key(&key)
        }
        "pin" | "unpin" => {
            let id = *p.get(1).ok_or("which app?")?;
            apps::set_pinned(id, p[0] == "pin")
        }
        "registry" => {
            print!("{}", apps::registry_text(Scope::System));
            print!("{}", apps::registry_text(Scope::User).trim_start_matches("gina-registry 1\n"));
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            Ok(())
        }
        other => Err(format!("unknown command {other}. Run pkg help.")),
    }
    .map_err(|e: String| e.to_string())
}
