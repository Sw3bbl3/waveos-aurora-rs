//! GINA package installation and app discovery, shared by Studio and Installer.
use crate::{fs, process};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

pub use gina::Manifest;
pub type Result<T> = core::result::Result<T, String>;
static INSTALL_LOCK: crate::sync::Mutex<()> = crate::sync::Mutex::new(());
#[derive(Clone)]
pub struct Installed {
    pub manifest: Manifest,
    pub executable: String,
    pub directory: String,
}

fn io(e: crate::Error) -> String {
    format!("Filesystem error: {e}")
}
fn mkdirs(path: &str) -> Result<()> {
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

pub fn installed(id: &str) -> Result<Installed> {
    if !gina::safe_path(id) || id.contains('/') {
        return Err("Invalid app ID".into());
    }
    let root = format!("/Applications/{id}");
    let active = fs::read_to_string(&format!("{root}/active")).map_err(io)?;
    let active = active.trim();
    if !gina::safe_path(active) || active.contains('/') {
        return Err("Invalid app registration".into());
    }
    let directory = format!("{root}/{active}");
    let text = fs::read_to_string(&format!("{directory}/manifest.toml")).map_err(io)?;
    let manifest = Manifest::parse(&text).map_err(ToString::to_string)?;
    if manifest.id != id {
        return Err("App registration does not match its manifest".into());
    }
    Ok(Installed { executable: format!("{directory}/{}", manifest.entry), manifest, directory })
}

pub fn list() -> Vec<Installed> {
    fs::read_dir("/Applications")
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.is_dir)
        .filter_map(|e| installed(&e.name).ok())
        .collect()
}

/// The active registration changes only after every package file is persisted.
pub fn install(bytes: &[u8], mut progress: impl FnMut(usize, usize)) -> Result<Installed> {
    let _install = INSTALL_LOCK.lock();
    let package = gina::Package::parse(bytes).map_err(ToString::to_string)?;
    let root = format!("/Applications/{}", package.manifest.id);
    mkdirs(&root)?;
    let generation = format!("{}-{}", package.manifest.version, package.fingerprint);
    let directory = format!("{root}/{generation}");
    // A staged generation cannot alter a running or previously installed app.
    let staging = format!("{root}/.pending-{}", process::pid());
    if fs::exists(&staging) {
        fs::remove_all(&staging).map_err(io)?;
    }
    mkdirs(&staging)?;
    let result = (|| {
        for (i, file) in package.files.iter().enumerate() {
            let path = format!("{staging}/{}", file.path);
            if let Some((parent, _)) = path.rsplit_once('/') {
                mkdirs(parent)?;
            }
            fs::write(&path, file.bytes).map_err(io)?;
            progress(i + 1, package.files.len());
        }
        fs::try_sync().map_err(io)?;
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
        mkdirs(&format!("/AppData/{}", package.manifest.id))?;
        let pending = format!("{root}/.active-{}", process::pid());
        fs::write(&pending, generation.as_bytes()).map_err(io)?;
        fs::try_sync().map_err(io)?;
        fs::rename(&pending, &format!("{root}/active")).map_err(io)?;
        fs::try_sync().map_err(io)?;
        installed(&package.manifest.id)
    })();
    if result.is_err() {
        let _ = fs::remove_all(&staging);
    }
    if result.is_ok() {
        refresh();
    }
    result
}

pub fn uninstall(id: &str, delete_data: bool) -> Result<()> {
    let _ = installed(id)?;
    // Deactivate first so interrupted cleanup cannot leave a launchable half-app.
    let root = format!("/Applications/{id}");
    fs::remove(&format!("{root}/active")).map_err(io)?;
    refresh();
    fs::remove_all(&root).map_err(io)?;
    if delete_data && fs::exists(&format!("/AppData/{id}")) {
        fs::remove_all(&format!("/AppData/{id}")).map_err(io)?;
    }
    fs::try_sync().map_err(io)?;
    Ok(())
}

pub fn launch(id: &str) -> Result<u32> {
    let app = installed(id)?;
    process::spawn(&app.executable, &[], process::Stdio::default()).map_err(io)
}

fn refresh() {
    let _ = crate::sys::call(crate::abi::nr::DESKTOP, &[crate::abi::desktop::REFRESH_APPS as u64, 0, 0]);
}
