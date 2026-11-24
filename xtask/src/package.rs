//! GINA packages on the host: `cargo xtask package`, publisher keys, the
//! sample packages (the WaveUI Gallery and Doom) and package repositories.
//!
//! Every checkout signs its sample packages and repository with its own key,
//! made on first use in `target/keys/` and never committed; the system image
//! trusts its public half (`/System/Keys/waveos-samples.pub`), so packages a
//! build makes install on that build without a warning.
use super::{root, Program};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The build's sample-publisher key.
pub const SAMPLE_KEY: &str = "target/keys/waveos-samples.key";
pub const SAMPLE_PUBLISHER: &str = "WaveOS Samples (this build)";

pub type Key = ([u8; 32], String);

fn random_seed() -> [u8; 32] {
    use std::io::Read;
    let mut seed = [0u8; 32];
    fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut seed)).expect("read /dev/urandom");
    seed
}

/// Writes a new key pair: `out` (secret, keep it private) and `out.pub`.
pub fn keygen(out: &Path, name: &str) -> Result<Key, String> {
    if out.exists() {
        return Err(format!("{} exists; not overwriting a key", out.display()));
    }
    if name.trim().is_empty() || name.len() > 100 || name.chars().any(char::is_control) {
        return Err("The publisher name must be 1-100 printable characters".into());
    }
    let seed = random_seed();
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(out, gina::sign::encode_secret(&seed, name)).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(out, fs::Permissions::from_mode(0o600));
    }
    let public = gina::PublicKey { key: gina::sign::public_key(&seed), name: name.into() };
    fs::write(pub_path(out), public.encode()).map_err(|e| e.to_string())?;
    Ok((seed, name.into()))
}

fn pub_path(secret: &Path) -> PathBuf {
    let mut p = secret.as_os_str().to_owned();
    p.push(".pub");
    PathBuf::from(p)
}

pub fn load_key(path: &Path) -> Result<Key, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    gina::sign::parse_secret(&text).map_err(str::to_string)
}

/// This checkout's sample key (made on first use).
pub fn sample_key() -> Key {
    let path = root().join(SAMPLE_KEY);
    if path.exists() {
        load_key(&path).expect("sample key")
    } else {
        keygen(&path, SAMPLE_PUBLISHER).expect("make the sample key")
    }
}

/// The public key file for the system image.
pub fn sample_public_key() -> String {
    let (seed, name) = sample_key();
    gina::PublicKey { key: gina::sign::public_key(&seed), name }.encode()
}

fn pack(m: &gina::Manifest, files: &[(&str, &[u8])], key: Option<&Key>) -> Result<Vec<u8>, String> {
    match key {
        Some((seed, _)) => {
            let sign = |msg: &[u8]| gina::sign::sign(seed, msg);
            gina::pack_with(m, files, Some(&sign))
        }
        None => gina::pack(m, files),
    }
    .map_err(str::to_string)
}

/// `cargo xtask package MANIFEST APP.elf OUTPUT.gina [--sign KEY]`
pub fn package(manifest: &Path, executable: &Path, out: &Path, key: Option<&Path>) -> Result<(), String> {
    let text = fs::read_to_string(manifest).map_err(|e| e.to_string())?;
    let m = gina::Manifest::parse(&text).map_err(str::to_string)?;
    let binary = fs::read(executable).map_err(|e| e.to_string())?;
    let root = manifest.parent().unwrap_or(Path::new("."));
    let mut resources = Vec::new();
    for path in m.resources.iter().chain(std::iter::once(&m.icon)).filter(|s| !s.is_empty()) {
        if !resources.iter().any(|(p, _)| p == path) {
            resources.push((path.clone(), fs::read(root.join(path)).map_err(|e| e.to_string())?));
        }
    }
    let mut files = vec![(m.entry.as_str(), binary.as_slice())];
    files.extend(resources.iter().map(|(p, b)| (p.as_str(), b.as_slice())));
    let key = key.map(load_key).transpose()?;
    let bytes = pack(&m, &files, key.as_ref())?;
    fs::write(out, &bytes).map_err(|e| e.to_string())?;
    let signed = key.as_ref().map(|(_, n)| format!(", signed by {n}")).unwrap_or_default();
    println!("Packaged {} {}: {} ({} bytes{signed})", m.name, m.version, out.display(), bytes.len());
    Ok(())
}

fn program<'a>(programs: &'a [Program], dest: &str) -> Option<&'a Path> {
    programs.iter().find(|(p, _)| p == dest).map(|(_, s)| s.as_path())
}

/// The WaveUI Gallery (the WaveSDK's example) at `version`.
pub fn gallery(programs: &[Program], version: &str, key: Option<&Key>) -> Option<Vec<u8>> {
    let elf = fs::read(program(programs, "Bin/gina-demo")?).ok()?;
    let icon = fs::read(root().join("assets/icons/studio.png")).ok()?;
    let m = gina::Manifest {
        format: 2,
        architecture: crate::arch::current().name().into(),
        id: "dev.example.gallery".into(),
        name: "WaveUI Gallery".into(),
        developer: "WaveSDK Example".into(),
        version: version.into(),
        entry: "bin/app".into(),
        icon: "Resources/icon.png".into(),
        description: "Every WaveUI control, in a few lines each.".into(),
        categories: vec!["developer".into()],
        min_os: "0.8".into(),
        ..Default::default()
    };
    Some(pack(&m, &[("bin/app", &elf), ("Resources/icon.png", &icon)], key).expect("package the gallery"))
}

/// Doom as a game package: the launcher app, the game (a Linux program)
/// and its license. Game data comes from Documents/Doom, as before.
pub fn doom(programs: &[Program], key: Option<&Key>) -> Option<Vec<u8>> {
    let launcher = fs::read(program(programs, "Apps/Doom.elf")?).ok()?;
    let mut game = fs::read(program(programs, "Games/Doom/doom")?).ok()?;
    // Mark the game a Linux program (ELF OSABI GNU/Linux): programs in
    // /Applications without a WaveOS note may otherwise be taken for native.
    if game.len() > 7 && game.starts_with(b"\x7fELF") {
        game[7] = 3;
    }
    let license = fs::read(root().join("third_party/doomgeneric/COPYING")).ok()?;
    let icon = fs::read(root().join("assets/icons/doom.png")).ok()?;
    let m = gina::Manifest {
        format: 2,
        architecture: crate::arch::current().name().into(),
        id: "org.waveos.doom".into(),
        name: "Doom".into(),
        developer: "WaveOS".into(),
        version: "1.0.0".into(),
        entry: "bin/app".into(),
        icon: "Resources/icon.png".into(),
        resources: vec!["Resources/COPYING-doomgeneric.txt".into()],
        extensions: vec!["wad".into()],
        description: "The classic shooter (doomgeneric). Plays Freedoom, or your own WAD files from Documents/Doom.".into(),
        categories: vec!["game".into()],
        permissions: vec!["network".into(), "documents".into(), "audio".into(), "gamepad".into(), "linux".into(), "spawn".into()],
        min_os: "0.8".into(),
        ..Default::default()
    };
    let files: [(&str, &[u8]); 4] = [
        ("bin/app", &launcher),
        ("bin/doom", &game),
        ("Resources/icon.png", &icon),
        ("Resources/COPYING-doomgeneric.txt", &license),
    ];
    Some(pack(&m, &files, key).expect("package Doom"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;
    use gina::repo::Index;
    use sha2::{Digest, Sha256};
    #[test]
    fn repository_round_trip() {
        let dir = std::env::temp_dir().join(format!("gina-repo-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let key = ([5u8; 32], String::from("Test"));
        let mut elf = vec![0u8; 121];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
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
            id: "dev.example.a".into(),
            name: "A".into(),
            developer: "Ex".into(),
            version: "1.0".into(),
            entry: "bin/app".into(),
            categories: vec!["game".into()],
            permissions: vec!["network".into()],
            ..Default::default()
        };
        let p = pack(&m, &[("bin/app", &elf)], Some(&key)).unwrap();
        let listing = store::Listing { featured: true, about: "About A".into(), ..Default::default() };
        // (An x86-64 program: the repository's x86-64 half, whatever the
        // host's default architecture.)
        let spec = || store::Spec { arch: crate::arch::Arch::X86_64, ..store::Spec::new("T", &key) };
        store::write(&dir, &spec(), &[(p.clone(), listing.clone())]).unwrap();
        // Format 1, for older systems.
        let text = fs::read(dir.join("index.txt")).unwrap();
        let sig = gina::Signature::parse(&fs::read_to_string(dir.join("index.sig")).unwrap()).unwrap();
        assert!(gina::sign::verify(&sig, &gina::repo::signed_message(&text)));
        let index = Index::parse(&text).unwrap();
        let e = index.latest("dev.example.a").unwrap();
        let bytes = fs::read(dir.join(&e.path)).unwrap();
        assert_eq!(bytes, p);
        assert_eq!(<[u8; 32]>::from(Sha256::digest(&bytes)), e.sha256);
        assert_eq!(gina::Package::parse(&bytes).unwrap().verify_signature(), Ok(Some(gina::sign::public_key(&key.0))));
        // The catalogue: signed, versioned, with the Store's details.
        let text = fs::read(dir.join("catalog.txt")).unwrap();
        let sigs = gina::repo::parse_signatures(&fs::read_to_string(dir.join("catalog.sig")).unwrap()).unwrap();
        let k = gina::sign::public_key(&key.0);
        assert!(gina::sign::verify_any(&sigs, &gina::repo::catalog_message(&text), &[k]).is_some());
        let c = Index::parse(&text).unwrap();
        assert_eq!((c.format, c.version, c.featured.clone()), (2, 1, vec![String::from("dev.example.a")]));
        assert_eq!(c.packages[0].about, "About A");
        assert_eq!(c.packages[0].permissions, ["network"]);
        // Writing again makes the next version.
        store::write(&dir, &spec(), &[(p, listing)]).unwrap();
        assert_eq!(store::current_catalog_for(&dir, crate::arch::Arch::X86_64).unwrap().version, 2);
        fs::remove_dir_all(&dir).unwrap();
    }
}
