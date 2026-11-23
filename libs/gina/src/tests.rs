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
            },
        ],
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
    aurora_mutate::check("gina-index", &[index().encode().into_bytes()], 5000, |bytes| {
        if let Ok(i) = repo::Index::parse(bytes) {
            assert_eq!(repo::Index::parse(i.encode().as_bytes()), Ok(i));
        }
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
