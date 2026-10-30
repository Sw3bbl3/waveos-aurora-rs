//! Versioned, data-only desktop links. No command lines are accepted.
use alloc::{
    format,
    string::{String, ToString},
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Builtin(String),
    Package(String),
    Path(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub target: Target,
}
fn identity(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}
impl Shortcut {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 4096 {
            return None;
        }
        let s = core::str::from_utf8(bytes).ok()?;
        let mut lines = s.lines();
        if lines.next()? != "WaveOS Shortcut 1" {
            return None;
        }
        let (kind, value) = lines.next()?.split_once('=')?;
        if lines.next().is_some() || value.chars().any(char::is_control) {
            return None;
        }
        let target = match kind {
            "builtin" if identity(value) => Target::Builtin(value.into()),
            "package" if identity(value) => Target::Package(value.into()),
            "path" if value.starts_with('/') && !value.split('/').any(|p| p == "..") => Target::Path(value.into()),
            _ => return None,
        };
        Some(Self { target })
    }
    pub fn encode(&self) -> Option<String> {
        let (kind, value) = match &self.target {
            Target::Builtin(v) => ("builtin", v),
            Target::Package(v) => ("package", v),
            Target::Path(v) => ("path", v),
        };
        let s = format!("WaveOS Shortcut 1\n{kind}={value}\n");
        Self::parse(s.as_bytes()).map(|_| s)
    }
    pub fn label(name: &str) -> String {
        name.strip_suffix(".wave-link").unwrap_or(name).to_string()
    }
}
/// Recognize catalog executables without treating arbitrary executables as apps.
pub fn builtin_for_path(path: &str) -> Option<&'static str> {
    Some(match path {
        "/System/Apps/Learn.elf" => "learn",
        "/System/Apps/Studio.elf" => "studio",
        "/System/Apps/Installer.elf" => "installer",
        "/System/Apps/Files.elf" => "files",
        "/System/NetSurf/netsurf" => "netsurf",
        "/System/Apps/Terminal.elf" => "terminal",
        "/System/Apps/Notes.elf" => "notes",
        "/System/Apps/Calculator.elf" => "calculator",
        "/System/Apps/Settings.elf" => "settings",
        "/System/Apps/Preview.elf" => "preview",
        "/System/Apps/Paint.elf" => "paint",
        "/System/Apps/Clock.elf" => "clock",
        "/System/Apps/Activity.elf" => "activity",
        "/System/Apps/Doom.elf" => "doom",
        "/System/Apps/Firefox.elf" => "firefox",
        "/System/Apps/Voyage.elf" => "voyage",
        "/System/Apps/Welcome.elf" => "welcome",
        "/System/Apps/About.elf" => "about",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        for target in [
            Target::Builtin("paint".into()),
            Target::Package("org.wave.app".into()),
            Target::Path("/Documents/My note.txt".into()),
        ] {
            let s = Shortcut { target };
            assert_eq!(Shortcut::parse(s.encode().unwrap().as_bytes()), Some(s));
        }
    }
    #[test]
    fn rejects_commands_versions_and_ambiguous_links() {
        for s in [
            "WaveOS Shortcut 2\npath=/a",
            "WaveOS Shortcut 1\nshell=rm -rf /",
            "WaveOS Shortcut 1\npath=/a\npath=/b",
            "WaveOS Shortcut 1\npath=../a",
            "WaveOS Shortcut 1\npackage=app --evil",
            "WaveOS Shortcut 1\npath=/a/../b",
        ] {
            assert!(Shortcut::parse(s.as_bytes()).is_none());
        }
    }
    #[test]
    fn package_link_has_no_version_path() {
        let s = Shortcut { target: Target::Package("editor".into()) };
        assert_eq!(s.encode().unwrap(), "WaveOS Shortcut 1\npackage=editor\n");
    }
}
