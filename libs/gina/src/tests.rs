use super::*;
use alloc::vec;

fn manifest() -> Manifest {
    Manifest {
        format: 1,
        id: "dev.example.hello".into(),
        name: "Hello".into(),
        developer: "Example".into(),
        version: "1.0.0".into(),
        entry: "bin/app".into(),
        ..Default::default()
    }
}

fn manifest2() -> Manifest {
    Manifest {
        format: 2,
        description: "Says hello, with a picture.".into(),
        categories: vec!["game".into(), "education".into()],
        permissions: vec!["network".into(), "documents".into()],
        min_os: "0.8".into(),
        dependencies: vec!["dev.example.runtime >= 1.2".into()],
        icon: "Resources/icon.png".into(),
        extensions: vec!["hello".into()],
        ..manifest()
    }
}

fn elf() -> Vec<u8> {
    let mut b = vec![0; 121];
    b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    b[16..18].copy_from_slice(&2u16.to_le_bytes());
    b[18..20].copy_from_slice(&62u16.to_le_bytes());
    b[24..32].copy_from_slice(&0x400000u64.to_le_bytes());
    b[32..40].copy_from_slice(&64u64.to_le_bytes());
    b[54..56].copy_from_slice(&56u16.to_le_bytes());
    b[56..58].copy_from_slice(&1u16.to_le_bytes());
    b[64..68].copy_from_slice(&1u32.to_le_bytes());
    b[68..72].copy_from_slice(&5u32.to_le_bytes());
    b[72..80].copy_from_slice(&120u64.to_le_bytes());
    b[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
    b[96..104].copy_from_slice(&1u64.to_le_bytes());
    b[104..112].copy_from_slice(&1u64.to_le_bytes());
    b[120] = 0xc3;
    b
}

const SECRET: [u8; 32] = [7; 32];

/// The start of a 16 by 16 PNG (all the icon check reads).
fn png() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend_from_slice(&16u32.to_be_bytes());
    b.extend_from_slice(&16u32.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0]);
    b
}

fn signer(s: &[u8]) -> Signature {
    sign::sign(&SECRET, s)
}

fn signed() -> Vec<u8> {
    let (e, i) = (elf(), png());
    pack_with(&manifest2(), &[("bin/app", &e), ("Resources/icon.png", &i), ("Resources/readme.txt", b"hi")], Some(&signer))
        .unwrap()
}

/// Rewrites the checksum of the header at `at`, so a test reaches the
/// checks behind the tar layer.
fn fix_checksum(b: &mut [u8], at: usize) {
    b[at + 148..at + 156].fill(b' ');
    let sum: usize = b[at..at + 512].iter().map(|v| *v as usize).sum();
    b[at + 148..at + 156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

/// Byte offsets of each entry's header, by path.
fn headers(b: &[u8]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 512 <= b.len() && b[at..at + 512].iter().any(|v| *v != 0) {
        let name = cstr(&b[at..at + 100]).unwrap().to_string();
        let size = octal(&b[at + 124..at + 136]).unwrap();
        out.push((name, at));
        at = (at + 512 + size).div_ceil(512) * 512;
    }
    out
}

fn header_of(b: &[u8], path: &str) -> usize {
    headers(b).into_iter().find(|(p, _)| p == path).unwrap().1
}

#[test]
fn roundtrip() {
    let m = manifest();
    let bytes = pack(&m, &[("bin/app", &elf())]).unwrap();
    let p = Package::parse(&bytes).unwrap();
    assert_eq!(p.manifest, m);
    assert_eq!(p.files.len(), 2);
    assert!(p.signature.is_none());
}

#[test]
fn format2_roundtrip_and_signature() {
    let bytes = signed();
    let p = Package::parse(&bytes).unwrap();
    assert_eq!(p.manifest, manifest2());
    assert!(p.manifest.is_game());
    assert_eq!(p.payload().count(), 3);
    assert_eq!(p.installed_size, p.files.iter().map(|f| f.bytes.len() as u64).sum::<u64>());
    assert_eq!(p.verify_signature(), Ok(Some(sign::public_key(&SECRET))));
    // Unsigned format 2 packages still carry (and check) their hashes.
    let unsigned = pack(&manifest2(), &[("bin/app", &elf()), ("Resources/icon.png", &png())]).unwrap();
    // Icons must be small PNG pictures.
    assert!(pack(&manifest2(), &[("bin/app", &elf()), ("Resources/icon.png", b"GIF89a")]).is_err());
    let mut huge = png();
    huge[16..20].copy_from_slice(&4096u32.to_be_bytes());
    assert!(pack(&manifest2(), &[("bin/app", &elf()), ("Resources/icon.png", &huge)]).is_err());
    let p = Package::parse(&unsigned).unwrap();
    assert_eq!(p.verify_signature(), Ok(None));
    assert_eq!(Manifest::parse(&manifest2().encode()), Ok(manifest2()));
}

#[test]
fn altered_payload_is_refused() {
    let mut b = signed();
    let at = header_of(&b, "Resources/readme.txt");
    b[at + 512] = b'H';
    assert_eq!(Package::parse(&b).unwrap_err(), "A payload file is damaged or was altered (hash mismatch)");
}

#[test]
fn altered_manifest_breaks_the_signature() {
    let mut b = signed();
    let at = header_of(&b, "manifest.toml");
    // Same length, still valid: the name changes from Hello to Jello.
    let pos = b[at + 512..].windows(7).position(|w| w == b"\"Hello\"").unwrap() + at + 512;
    b[pos + 1] = b'J';
    let p = Package::parse(&b).unwrap();
    assert_eq!(p.manifest.name, "Jello");
    assert!(p.verify_signature().is_err());
}

#[test]
fn swapped_signature_key_fails() {
    let b = signed();
    let p = Package::parse(&b).unwrap();
    let mut s = p.signature.unwrap();
    s.key = sign::public_key(&[9; 32]);
    assert!(!sign::verify(&s, &p.signed_message()));
}

#[test]
fn unlisted_and_missing_files_are_refused() {
    // A file the hash list does not name.
    let mut b = signed();
    let at = header_of(&b, "Resources/readme.txt");
    b[at + 10] = b'X'; // Resources/rXadme.txt
    fix_checksum(&mut b, at);
    assert!(Package::parse(&b).is_err());
    // Extra files in META.
    let e = elf();
    assert!(pack(&manifest2(), &[("bin/app", &e), ("META/extra", b"x")]).is_err());
    let mut b = signed();
    let at = header_of(&b, "Resources/readme.txt");
    b[at..at + 20].copy_from_slice(b"META/readme.txt\0\0\0\0\0");
    fix_checksum(&mut b, at);
    assert_eq!(Package::parse(&b).unwrap_err(), "Unexpected file in META");
}

#[test]
fn reject_bad_inputs() {
    let m = manifest();
    assert!(pack(&m, &[("../app", &elf())]).is_err());
    assert!(pack(&m, &[("bin/app", b"not an ELF")]).is_err());
    assert!(Manifest::parse(&(m.encode() + "built_in = true\n")).is_err());
    assert!(Manifest::parse(&(m.encode() + "id = \"other.app\"\n")).is_err());
    // Format 2 keys are not format 1 keys.
    assert!(Manifest::parse(&(m.encode() + "categories = [\"game\"]\n")).is_err());
    let mut bytes = pack(&m, &[("bin/app", &elf())]).unwrap();
    bytes[0] ^= 1;
    assert!(Package::parse(&bytes).is_err());
}

#[test]
fn hostile_paths() {
    let base = pack(&manifest(), &[("bin/app", &elf()), ("data/file.txt", b"x")]).unwrap();
    let at = header_of(&base, "data/file.txt");
    for evil in [&b"/etc/passwd"[..], b"data/../../x", b"data/./x", b"data//x", b"C:/x", b"data\\x", b"data/\x01"] {
        let mut b = base.clone();
        b[at..at + 100].fill(0);
        b[at..at + evil.len()].copy_from_slice(evil);
        fix_checksum(&mut b, at);
        assert!(Package::parse(&b).is_err(), "{:?}", core::str::from_utf8(evil));
    }
    // A prefix field that climbs out.
    let mut b = base.clone();
    b[at + 345..at + 348].copy_from_slice(b"../");
    fix_checksum(&mut b, at);
    assert!(Package::parse(&b).is_err());
    // Case-folded duplicates.
    let e = elf();
    assert_eq!(
        pack(&manifest(), &[("bin/app", &e), ("Data/a", b"1"), ("data/A", b"2")]).unwrap_err(),
        "Duplicate archive path"
    );
    // Too deep.
    let deep = "a/".repeat(MAX_DEPTH) + "f";
    assert!(pack(&manifest(), &[("bin/app", &e), (&deep, b"1")]).is_err());
    // A size that runs past the archive.
    let mut b = base.clone();
    b[at + 124..at + 136].copy_from_slice(b"77777777777\0");
    fix_checksum(&mut b, at);
    assert!(Package::parse(&b).is_err());
}

#[test]
fn rejects_links_even_with_valid_checksum() {
    for kind in *b"12346LKxg" {
        let mut b = pack(&manifest(), &[("bin/app", &elf())]).unwrap();
        b[156] = kind;
        fix_checksum(&mut b, 0);
        assert!(Package::parse(&b).is_err());
    }
}

#[test]
fn manifest_validation() {
    let bad = |f: &dyn Fn(&mut Manifest)| {
        let mut m = manifest2();
        f(&mut m);
        Manifest::parse(&m.encode()).is_err()
    };
    assert!(bad(&|m| m.categories = vec!["weapons".into()]));
    assert!(bad(&|m| m.permissions = vec!["root".into()]));
    assert!(bad(&|m| m.permissions = vec!["network".into(), "network".into()]));
    assert!(bad(&|m| m.version = "1.0.x".into()));
    assert!(bad(&|m| m.min_os = "soon".into()));
    assert!(bad(&|m| m.dependencies = vec!["dev.example.hello".into()]));
    assert!(bad(&|m| m.dependencies = vec!["a.b".into(), "a.b >= 2".into()]));
    assert!(bad(&|m| m.entry = "META/app".into()));
    assert!(bad(&|m| m.id = "NoDots".into()));
    assert!(bad(&|m| m.name = " ".into()));
    assert_eq!(describe_permission("network"), "Connect to the internet and your local network");
    assert_eq!(manifest2().dependencies()[0].id, "dev.example.runtime");
}

/// The sandbox's permissions: packages may declare the new ones, never the
/// system's own (only the manifests built into the system may).
#[test]
fn permissions_for_the_sandbox() {
    let with = |perms: &[&str]| Manifest { permissions: perms.iter().map(|p| (*p).into()).collect(), ..manifest2() };
    let text = with(&["camera", "screen-capture", "launch", "ipc", "background", "desktop:read"]).encode();
    assert!(Manifest::parse(&text).is_ok());
    for system in ["system", "admin", "privacy", "processes", "linux-home", "all-files"] {
        let text = with(&["network", system]).encode();
        assert_eq!(Manifest::parse(&text), Err("Unknown permission"), "{system} in a package");
        assert!(Manifest::parse_first_party(&text).is_ok(), "{system} in the system's own manifest");
    }
    assert!(Manifest::parse_first_party(&with(&["root"]).encode()).is_err());
    assert!(sensitive_permission("camera") && !sensitive_permission("launch"));
    assert_eq!(describe_permission("all-files"), "Read and change every file you may");
}

#[test]
fn installed_file_verification() {
    let bytes = signed();
    let p = Package::parse(&bytes).unwrap();
    let hashes = core::str::from_utf8(p.files.iter().find(|f| f.path == HASHES).unwrap().bytes).unwrap();
    let read = |path: &str| p.files.iter().find(|f| f.path == path).map(|f| f.bytes.to_vec());
    assert_eq!(verify_files(hashes, read), Ok(vec![]));
    let changed = |path: &str| {
        let mut b = p.files.iter().find(|f| f.path == path).map(|f| f.bytes.to_vec())?;
        if path == "Resources/readme.txt" {
            b[0] ^= 1;
        }
        Some(b)
    };
    assert_eq!(verify_files(hashes, changed), Ok(vec![String::from("Resources/readme.txt")]));
    assert!(verify_files("zz 1 ../x\n", |_| None).is_err());
}

#[test]
fn keys_and_signatures_text() {
    let k = PublicKey { key: sign::public_key(&SECRET), name: "Example Publisher".into() };
    assert_eq!(PublicKey::parse(&k.encode()), Ok(k.clone()));
    assert!(PublicKey::parse("ed25519 abcd Name").is_err());
    let s = signer(b"x");
    assert_eq!(Signature::parse(&s.encode()), Ok(s));
    assert!(Signature::parse("rsa 00 00").is_err());
    let (seed, name) = sign::parse_secret(&sign::encode_secret(&SECRET, "Example")).unwrap();
    assert_eq!((seed, name.as_str()), (SECRET, "Example"));
    assert_eq!(fingerprint(&k.key).len(), 19);
}

fn index() -> repo::Index {
    repo::Index {
        name: "Samples".into(),
        generated: 1_700_000_000,
        packages: vec![
            repo::Entry {
                id: "dev.example.hello".into(),
                version: "1.0.0".into(),
                sha256: [1; 32],
                size: 4096,
                path: "packages/dev.example.hello-1.0.0.gina".into(),
                categories: vec!["game".into()],
                name: "Hello".into(),
                author: "Example".into(),
                description: "Says hello".into(),
                ..Default::default()
            },
            repo::Entry {
                id: "dev.example.hello".into(),
                version: "1.10.0".into(),
                sha256: [2; 32],
                size: 4096,
                path: "packages/dev.example.hello-1.10.0.gina".into(),
                categories: vec![],
                name: "Hello".into(),
                author: "Example".into(),
                description: String::new(),
                ..Default::default()
            },
        ],
        format: 1,
        ..Default::default()
    }
}

#[test]
fn repository_index() {
    let i = index();
    let text = i.encode();
    assert_eq!(repo::Index::parse(text.as_bytes()), Ok(i.clone()));
    assert_eq!(i.latest("dev.example.hello").unwrap().version, "1.10.0");
    assert_eq!(i.find("dev.example.hello", "1.0").unwrap().sha256, [1; 32]);
    assert_eq!(i.search("hello").len(), 1);
    assert_eq!(i.search("nothing").len(), 0);
    let sig = sign::sign(&SECRET, &repo::signed_message(text.as_bytes()));
    assert!(sign::verify(&sig, &repo::signed_message(text.as_bytes())));
    let tampered = text.replace("1.10.0", "1.11.0");
    assert!(!sign::verify(&sig, &repo::signed_message(tampered.as_bytes())));
    for bad in [
        text.replace("gina-repository 1", "gina-repository 2"),
        text.replace("packages/dev", "../dev"),
        text.replace("\t4096\t", "\t0\t"),
        text.clone() + text.lines().last().unwrap() + "\n",
        text.replace("generated", "made"),
        text.replace("Says hello", "Says\u{7}hello"),
    ] {
        assert!(repo::Index::parse(bad.as_bytes()).is_err(), "{bad}");
    }
}

fn catalog() -> repo::Index {
    let mut i = index();
    i.format = 2;
    i.version = 7;
    i.expires = 1_800_000_000;
    i.mirrors = vec!["https://mirror.example/stable/".into()];
    i.featured = vec!["dev.example.hello".into()];
    let e = &mut i.packages[1];
    e.installed_size = 12_345;
    e.permissions = vec!["network".into(), "documents:read".into()];
    e.min_os = "0.8".into();
    e.published = 1_700_000_100;
    e.icon = Some(repo::Asset { path: format!("assets/{}.png", hex(&[3; 32])), sha256: [3; 32], size: 900 });
    e.screenshots = vec![repo::Asset { path: "assets/shot.png".into(), sha256: [4; 32], size: 9000 }];
    e.about = "Line one\nLine two\twith a tab and a \\ backslash".into();
    e.notes = "Faster.".into();
    i.revoked = vec![("dev.example.hello".into(), "1.10.0".into())];
    i.system = vec![repo::SystemRelease {
        version: "0.9.0".into(),
        path: "system/0.9.0/update.txt".into(),
        sha256: [5; 32],
        size: 2000,
        min_from: "0.8".into(),
        published: 1_700_000_200,
    }];
    i
}

#[test]
fn repository_catalog() {
    let c = catalog();
    let text = c.encode();
    assert!(text.starts_with("gina-repository 2\n"));
    assert_eq!(repo::Index::parse(text.as_bytes()), Ok(c.clone()));
    // Format 1 keeps only what older systems understand.
    let v1 = repo::Index::parse(c.encode_v1().as_bytes()).unwrap();
    assert_eq!((v1.format, v1.packages.len(), v1.packages[1].installed_size), (1, 2, 0));
    // A revoked version is never the latest.
    assert_eq!(c.latest("dev.example.hello").unwrap().version, "1.0.0");
    assert!(c.is_revoked("dev.example.hello", "1.10"));
    assert_eq!(c.versions("dev.example.hello")[0].version, "1.10.0");
    assert_eq!(c.system_update("0.8.0").unwrap().version, "0.9.0");
    assert!(c.system_update("0.9.0").is_none() && c.system_update("0.7").is_none());
    for bad in [
        text.replace("version\t7\n", ""),
        text.replace("expires\t1800000000\n", ""),
        text.replace("version\t7", "version\t0"),
        text.replace("https://mirror", "ftp://mirror"),
        text.replace("documents:read", "everything"),
        text.replace("icon\tdev.example.hello\t1.10.0", "icon\tdev.example.other\t1.10.0"),
        text.replace("\\n", "\\q"),
        text.replace("\t2000\t0.8", "\t0\t0.8"),
        text.replace("featured\tdev.example.hello", "featured\tNot An Id"),
    ] {
        assert!(repo::Index::parse(bad.as_bytes()).is_err(), "{bad}");
    }
    assert_eq!(repo::unescape(&repo::escape("a\\b\nc\td"), 100).unwrap(), "a\\b\nc\td");
}

#[test]
fn catalog_freshness() {
    let c = catalog();
    let d = [9u8; 32];
    let none = repo::Seen::default();
    assert!(repo::check_fresh(&c, &d, 1_700_000_000, &none).is_ok());
    // Expired: a frozen mirror.
    let e = repo::check_fresh(&c, &d, 1_800_000_001, &none).unwrap_err();
    assert!(e.contains("expired"), "{e}");
    // Older than seen: a rollback.
    let seen = repo::Seen { version: 8, digest: [1; 32], generated: 0 };
    assert!(repo::check_fresh(&c, &d, 1_700_000_000, &seen).unwrap_err().contains("rollback"));
    // Same version, different contents.
    let seen = repo::Seen { version: 7, digest: [1; 32], generated: 0 };
    assert!(repo::check_fresh(&c, &d, 1_700_000_000, &seen).is_err());
    let seen = repo::Seen { version: 7, digest: d, generated: 0 };
    assert!(repo::check_fresh(&c, &d, 1_700_000_000, &seen).is_ok());
    assert!(repo::check_fresh(&index(), &d, 0, &none).is_err());
}

#[test]
fn trust_roots_rotate() {
    let (old, new, cat) = ([11u8; 32], [12u8; 32], [13u8; 32]);
    let key = |s: &[u8; 32]| sign::public_key(s);
    let root = |v: u64, root_key: &[u8; 32]| repo::Root {
        version: v,
        expires: 0,
        keys: vec![
            (repo::Role::Root, key(root_key), "Root".into()),
            (repo::Role::Catalog, key(&cat), "Catalogue".into()),
        ],
    };
    let r1 = root(1, &old);
    assert_eq!(repo::Root::parse(r1.encode().as_bytes()), Ok(r1.clone()));
    let r2 = root(2, &new);
    let text = r2.encode();
    let msg = repo::root_message(text.as_bytes());
    let both = [sign::sign(&old, &msg), sign::sign(&new, &msg)];
    assert_eq!(sign::advance_root(&r1, text.as_bytes(), &both), Ok(r2.clone()));
    // Only the new key, only the old key, or the wrong version: refused.
    assert!(sign::advance_root(&r1, text.as_bytes(), &both[1..]).is_err());
    assert!(sign::advance_root(&r1, text.as_bytes(), &both[..1]).is_err());
    assert!(sign::advance_root(&r2, text.as_bytes(), &both).is_err());
    // A catalogue signed by the catalogue key verifies against the root.
    let catalog = catalog().encode();
    let sig = sign::sign(&cat, &repo::catalog_message(catalog.as_bytes()));
    let sigs = repo::parse_signatures(&(sign::sign(&old, b"other").encode() + &sig.encode())).unwrap();
    let m = repo::catalog_message(catalog.as_bytes());
    assert_eq!(sign::verify_any(&sigs, &m, &r2.keys(repo::Role::Catalog)), Some(key(&cat)));
    assert_eq!(sign::verify_any(&sigs, &m, &r2.keys(repo::Role::Root)), None);
    for bad in [text.replace("version\t2", "version\t0"), text.replace("key\tcatalog", "key\tmaster")] {
        assert!(repo::Root::parse(bad.as_bytes()).is_err());
    }
    assert!(repo::parse_signatures("").is_err());
}

/// A small system image as the build writes it.
fn system_tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&system::tar_header("Apps", 0, true));
    for (name, data) in files {
        out.extend_from_slice(&system::tar_header(name, data.len() as u64, false));
        out.extend_from_slice(data);
        out.resize(out.len() + system::padding(data.len() as u64), 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

#[test]
fn system_update_rebuilds_the_image() {
    let old = system_tar(&[("version.txt", b"WaveOS Aurora 0.8.0\n"), ("Apps/Files.elf", &[7; 1000]), ("empty", b"")]);
    let new = system_tar(&[("version.txt", b"WaveOS Aurora 0.8.1\n"), ("Apps/Files.elf", &[7; 1000]), ("empty", b"")]);
    let part = |b: &[u8]| system::Part { sha256: Sha256::digest(b).into(), size: b.len() as u64 };
    let u = system::describe("0.8.1", "0.8", "Fixes.\nMore fixes.", part(b"kernel"), part(b"boot"), &new).unwrap();
    assert_eq!(system::Update::parse(u.encode().as_bytes()), Ok(u.clone()));
    assert_eq!(system::tar_size(&u.layout), new.len() as u64);
    // Only what changed is missing: the new version.txt, kernel and bootloader.
    let have: Vec<[u8; 32]> = system::layout_of(&old)
        .unwrap()
        .into_iter()
        .filter_map(|(i, _)| match i {
            system::Item::File { sha256, .. } => Some(sha256),
            _ => None,
        })
        .collect();
    let missing = u.missing(|s| have.contains(s));
    assert_eq!(missing.len(), 3);
    assert!(missing.contains(&(Sha256::digest(b"WaveOS Aurora 0.8.1\n").into(), 20)));
    // Rebuilding from the entries gives the same bytes.
    let mut rebuilt = Vec::new();
    let objects: Vec<(system::Item, core::ops::Range<usize>)> = system::layout_of(&new).unwrap();
    for item in &u.layout {
        match item {
            system::Item::Dir(p) => rebuilt.extend_from_slice(&system::tar_header(p, 0, true)),
            system::Item::File { path, size, .. } => {
                rebuilt.extend_from_slice(&system::tar_header(path, *size, false));
                let (_, r) = objects.iter().find(|(i, _)| i.path() == path).unwrap();
                rebuilt.extend_from_slice(&new[r.clone()]);
                rebuilt.resize(rebuilt.len() + system::padding(*size), 0);
            }
        }
    }
    rebuilt.resize(rebuilt.len() + 1024, 0);
    assert_eq!(rebuilt, new);
    let text = u.encode();
    for bad in [
        text.replace("gina-system-update 1", "gina-system-update 2"),
        text.replace("kernel\t", "kernal\t"),
        text.replace("\t1000\n", "\t2000\n"),
        text.replace("Apps/Files.elf", "../Files.elf"),
        text.lines().filter(|l| !l.starts_with("bootloader")).map(|l| format!("{l}\n")).collect(),
    ] {
        assert!(system::Update::parse(bad.as_bytes()).is_err(), "{bad}");
    }
    // Images with other headers, or that end early, can't be rebuilt.
    let mut odd = new.clone();
    odd[100..108].copy_from_slice(b"0000777\0");
    assert!(system::layout_of(&odd).is_err());
    assert!(system::layout_of(&new[..new.len() - 512]).is_err());
}

/// Packages and indexes come from anywhere; a corrupt one is refused, never a panic.
#[test]
fn corrupt_inputs_never_panic() {
    let mut m = manifest();
    m.resources.push("Resources/readme.txt".into());
    let package = pack(&m, &[("bin/app", &elf()), ("Resources/readme.txt", b"hello")]).unwrap();
    aurora_mutate::check("gina", &[package, signed()], 3000, |bytes| {
        if let Ok(p) = Package::parse(bytes) {
            let _ = p.verify_signature();
        }
    });
    aurora_mutate::check("gina-manifest", &[m.encode().into_bytes(), manifest2().encode().into_bytes()], 5000, |text| {
        if let Ok(m) = Manifest::parse(&String::from_utf8_lossy(text)) {
            assert_eq!(Manifest::parse(&m.encode()), Ok(m));
        }
    });
    aurora_mutate::check("gina-index", &[index().encode().into_bytes(), catalog().encode().into_bytes()], 5000, |bytes| {
        if let Ok(i) = repo::Index::parse(bytes) {
            assert_eq!(repo::Index::parse(i.encode().as_bytes()), Ok(i));
        }
    });
    let root = repo::Root { version: 3, expires: 9, keys: vec![(repo::Role::Root, [1; 32], "R".into()), (repo::Role::Catalog, [2; 32], "C".into())] };
    aurora_mutate::check("gina-root", &[root.encode().into_bytes()], 3000, |bytes| {
        if let Ok(r) = repo::Root::parse(bytes) {
            assert_eq!(repo::Root::parse(r.encode().as_bytes()), Ok(r));
        }
    });
    let tar = system_tar(&[("version.txt", b"WaveOS Aurora 0.8.1\n"), ("Apps/A.elf", &[1; 700])]);
    let part = system::Part { sha256: [1; 32], size: 10 };
    let update = system::describe("0.8.1", "0.8", "Notes", part, part, &tar).unwrap();
    aurora_mutate::check("gina-system-update", &[update.encode().into_bytes()], 3000, |bytes| {
        if let Ok(u) = system::Update::parse(bytes) {
            assert_eq!(system::Update::parse(u.encode().as_bytes()), Ok(u));
        }
    });
    aurora_mutate::check("gina-system-image", &[tar], 2000, |bytes| {
        let _ = system::layout_of(bytes);
    });
    aurora_mutate::check("gina-records", &[signer(b"x").encode().into_bytes()], 2000, |bytes| {
        let s = String::from_utf8_lossy(bytes);
        let _ = Signature::parse(&s);
        let _ = PublicKey::parse(&s);
        let _ = sign::parse_secret(&s);
        let _ = verify_files(&s, |_| None);
        let _ = version::compare(&s, "1.0");
        let _ = Dependency::parse(&s);
    });
}

#[test]
fn required_resources_and_abi() {
    let mut m = manifest();
    m.resources.push("missing.png".into());
    assert!(pack(&m, &[("bin/app", &elf())]).is_err());
    assert!(Manifest::parse(&manifest().encode().replace("abi = 1", "abi = 99")).is_err());
    assert!(Manifest::parse(&manifest().encode().replace("format = 1", "format = 3")).is_err());
}
