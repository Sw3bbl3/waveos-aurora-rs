use std::{fs, path::Path};
pub fn package(manifest: &Path, executable: &Path, out: &Path) -> Result<(), String> {
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
    let bytes = gina::pack(&m, &files).map_err(str::to_string)?;
    fs::write(out, &bytes).map_err(|e| e.to_string())?;
    println!("Packaged {} {}: {} ({} bytes)", m.name, m.version, out.display(), bytes.len());
    Ok(())
}
