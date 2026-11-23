//! What Wave Play shows: games, apps and settings shortcuts, gathered from
//! sources that each know one place things are installed.
//!
//! An [`Entry`] is a title, artwork and a way to start it. [`sources`] lists
//! the providers; adding a kind of library (a package manager, a store)
//! means adding one, or simply dropping files where [`Manifests`] looks:
//!
//! ```text
//! /Settings/Play/Library/<anything>.play     (key=value lines)
//! id=org.example.game
//! title=Example Game
//! kind=game            (game, app or setting)
//! exec=/Linux/usr/games/example
//! args=--fullscreen    (optional, space-separated)
//! art=/Linux/usr/share/example/cover.png   (optional, PNG/JPEG/GIF/BMP)
//! detail=Installed with the package manager (optional)
//! ```

use crate::gui::apps::{AppKind, CATALOG};
use crate::gui::icons::Icon;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Game,
    App,
    Setting,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Launch {
    /// An app in the desktop's catalog.
    App(AppKind),
    /// Any program (WaveOS or Linux), with arguments.
    Program(String, Vec<String>),
    /// A Settings pane (`Settings.elf <pane>`).
    Settings(&'static str),
    /// Back to the desktop.
    Desktop,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Art {
    Icon(Icon),
    Picture(String),
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// Stable across boots (recently played is kept by id).
    pub id: String,
    pub title: String,
    /// One line under the title: where it comes from, or what it does.
    pub detail: String,
    pub kind: Kind,
    pub art: Art,
    pub launch: Launch,
}

/// Where entries come from.
pub trait Source {
    fn collect(&self, out: &mut Vec<Entry>);
}

/// Every source, in the order their entries appear.
pub fn sources() -> [&'static dyn Source; 5] {
    [&Builtin, &Manifests, &Installed, &LinuxGames, &Steam]
}

/// The library as it is now (sources read the disk; call on entering).
pub fn gather() -> Vec<Entry> {
    let mut out = Vec::new();
    for s in sources() {
        s.collect(&mut out);
    }
    // First of each id wins (a manifest can't shadow a built-in game).
    let mut seen: Vec<String> = Vec::new();
    out.retain(|e| {
        let new = !seen.contains(&e.id);
        if new {
            seen.push(e.id.clone());
        }
        new
    });
    out.extend(shortcuts());
    out
}

/// The built-in catalog: Doom is a game, the rest are apps.
struct Builtin;

impl Source for Builtin {
    fn collect(&self, out: &mut Vec<Entry>) {
        for a in CATALOG.iter().filter(|a| a.listed && !a.path.is_empty()) {
            let game = a.kind == AppKind::Doom;
            out.push(Entry {
                id: format!("app:{}", a.name),
                title: String::from(a.name),
                detail: String::from(if game { "Freedoom or your own WAD files" } else { "WaveOS app" }),
                kind: if game { Kind::Game } else { Kind::App },
                art: Art::Icon(a.icon),
                launch: Launch::App(a.kind),
            });
        }
    }
}

/// Apps installed from `.gina` packages.
struct Installed;

impl Source for Installed {
    fn collect(&self, out: &mut Vec<Entry>) {
        for a in crate::gui::installed::list() {
            let base = crate::fs::parent_path(&a.path);
            let icon = format!("{}/{}", base, a.manifest.icon);
            out.push(Entry {
                id: format!("gina:{}", a.manifest.id),
                title: a.manifest.name.clone(),
                detail: if a.manifest.developer.is_empty() {
                    String::from("Installed app")
                } else {
                    a.manifest.developer.clone()
                },
                kind: Kind::App,
                art: if a.manifest.icon.is_empty() { Art::Icon(Icon::Installer) } else { Art::Picture(icon) },
                launch: Launch::Program(a.path, Vec::new()),
            });
        }
    }
}

/// `key=value` files in `/Settings/Play/Library` (see the module docs).
struct Manifests;

pub const MANIFESTS: &str = "/Settings/Play/Library";

impl Source for Manifests {
    fn collect(&self, out: &mut Vec<Entry>) {
        for f in crate::fs::readdir(MANIFESTS).unwrap_or_default() {
            if !f.name.ends_with(".play") || f.size > 4096 {
                continue;
            }
            let path = format!("{MANIFESTS}/{}", f.name);
            let Some(text) = crate::fs::read_all(&path).ok().and_then(|b| String::from_utf8(b).ok()) else {
                continue;
            };
            if let Some(e) = parse_manifest(&text) {
                out.push(e);
            }
        }
    }
}

fn parse_manifest(text: &str) -> Option<Entry> {
    let get = |k: &str| {
        text.lines().find_map(|l| {
            let (key, v) = l.split_once('=')?;
            (key.trim() == k).then(|| String::from(v.trim()))
        })
    };
    let exec = get("exec").filter(|e| e.starts_with('/'))?;
    let title = get("title").filter(|t| !t.is_empty())?;
    let kind = match get("kind").as_deref() {
        Some("app") => Kind::App,
        Some("setting") => Kind::Setting,
        _ => Kind::Game,
    };
    let args = get("args").map(|a| a.split_whitespace().map(String::from).collect()).unwrap_or_default();
    Some(Entry {
        id: get("id").unwrap_or_else(|| format!("play:{exec}")),
        title,
        detail: get("detail").unwrap_or_else(|| String::from("Added to your library")),
        kind,
        art: get("art").filter(|a| a.starts_with('/')).map(Art::Picture).unwrap_or(Art::Icon(Icon::Doom)),
        launch: Launch::Program(exec, args),
    })
}

/// Linux games: freedesktop `.desktop` files in the Games category, from the
/// disk's Linux packages (`/Linux`) and the system image's.
struct LinuxGames;

const LINUX_ROOTS: [&str; 2] = ["/Linux", "/System/Linux"];

/// Where a Linux path (`/usr/games/x`) is, seen from WaveOS.
fn linux_path(path: &str) -> Option<String> {
    LINUX_ROOTS.iter().map(|r| format!("{r}{path}")).find(|p| crate::fs::stat(p).is_ok())
}

impl Source for LinuxGames {
    fn collect(&self, out: &mut Vec<Entry>) {
        for root in LINUX_ROOTS {
            let dir = format!("{root}/usr/share/applications");
            for f in crate::fs::readdir(&dir).unwrap_or_default() {
                if !f.name.ends_with(".desktop") || f.size > 64 * 1024 {
                    continue;
                }
                let Some(text) =
                    crate::fs::read_all(&format!("{dir}/{}", f.name)).ok().and_then(|b| String::from_utf8(b).ok())
                else {
                    continue;
                };
                if let Some(e) = parse_desktop_file(&text, &f.name) {
                    out.push(e);
                }
            }
        }
    }
}

fn parse_desktop_file(text: &str, file: &str) -> Option<Entry> {
    let mut in_entry = false;
    let (mut name, mut exec, mut icon, mut categories, mut hidden) = (None, None, None, String::new(), false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        let Some((k, v)) = line.split_once('=').filter(|_| in_entry) else { continue };
        match k.trim() {
            "Name" => name = Some(String::from(v.trim())),
            "Exec" => exec = Some(String::from(v.trim())),
            "Icon" => icon = Some(String::from(v.trim())),
            "Categories" => categories = String::from(v),
            "NoDisplay" | "Hidden" => hidden |= v.trim() == "true",
            _ => {}
        }
    }
    if hidden || !categories.split(';').any(|c| c == "Game") {
        return None;
    }
    // The program and its arguments, without the field codes (%f, %U, …).
    let mut words = exec?.split_whitespace().filter(|w| !w.starts_with('%')).map(String::from).collect::<Vec<_>>();
    if words.is_empty() {
        return None;
    }
    let program = words.remove(0);
    let program = if program.starts_with('/') {
        linux_path(&program)?
    } else {
        ["/usr/games/", "/usr/bin/"].iter().find_map(|d| linux_path(&format!("{d}{program}")))?
    };
    let art = icon.and_then(|i| {
        if i.starts_with('/') {
            linux_path(&i)
        } else {
            ["/usr/share/icons/hicolor/256x256/apps/", "/usr/share/icons/hicolor/128x128/apps/", "/usr/share/pixmaps/"]
                .iter()
                .find_map(|d| linux_path(&format!("{d}{i}.png")))
        }
    });
    Some(Entry {
        id: format!("linux:{}", file.trim_end_matches(".desktop")),
        title: name?,
        detail: String::from("Linux game"),
        kind: Kind::Game,
        art: art.map(Art::Picture).unwrap_or(Art::Icon(Icon::Doom)),
        launch: Launch::Program(program, words),
    })
}

/// Steam, when the disk's Linux packages have it: a tile for Steam itself
/// (Big Picture), and one per installed game from its library manifests.
struct Steam;

impl Steam {
    fn program() -> Option<String> {
        ["/usr/games/steam", "/usr/bin/steam"].iter().find_map(|p| linux_path(p))
    }

    /// Steam libraries in the Linux home folders.
    fn libraries() -> Vec<String> {
        let mut homes: Vec<String> = alloc::vec![String::from("/Linux/root")];
        for h in crate::fs::readdir("/Linux/home").unwrap_or_default() {
            homes.push(format!("/Linux/home/{}", h.name));
        }
        homes
            .iter()
            .flat_map(|h| [format!("{h}/.local/share/Steam"), format!("{h}/.steam/steam")])
            .filter(|p| crate::fs::stat(&format!("{p}/steamapps")).is_ok())
            .collect()
    }
}

/// The value of `"key"  "value"` in a Valve KeyValues file.
fn vdf_value(text: &str, key: &str) -> Option<String> {
    let quoted = format!("\"{key}\"");
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix(&quoted)?;
        let v = rest.trim().strip_prefix('"')?.strip_suffix('"')?;
        Some(String::from(v))
    })
}

impl Source for Steam {
    fn collect(&self, out: &mut Vec<Entry>) {
        let Some(steam) = Self::program() else { return };
        out.push(Entry {
            id: String::from("steam"),
            title: String::from("Steam"),
            detail: String::from("Big Picture"),
            kind: Kind::Game,
            art: Art::Icon(Icon::Installer),
            launch: Launch::Program(steam.clone(), alloc::vec![String::from("-bigpicture")]),
        });
        for lib in Self::libraries() {
            for f in crate::fs::readdir(&format!("{lib}/steamapps")).unwrap_or_default() {
                if !f.name.starts_with("appmanifest_") || f.size > 64 * 1024 {
                    continue;
                }
                let Some(text) = crate::fs::read_all(&format!("{lib}/steamapps/{}", f.name))
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                else {
                    continue;
                };
                let (Some(id), Some(name)) = (vdf_value(&text, "appid"), vdf_value(&text, "name")) else { continue };
                let cover = format!("{lib}/appcache/librarycache/{id}_library_600x900.jpg");
                out.push(Entry {
                    id: format!("steam:{id}"),
                    title: name,
                    detail: String::from("Steam"),
                    kind: Kind::Game,
                    art: if crate::fs::stat(&cover).is_ok() { Art::Picture(cover) } else { Art::Icon(Icon::Installer) },
                    launch: Launch::Program(steam.clone(), alloc::vec![format!("steam://rungameid/{id}")]),
                });
            }
        }
    }
}

/// The Settings row.
fn shortcuts() -> Vec<Entry> {
    let s = |id: &str, title: &str, detail: &str, pane: &'static str| Entry {
        id: format!("settings:{id}"),
        title: String::from(title),
        detail: String::from(detail),
        kind: Kind::Setting,
        art: Art::Icon(Icon::Settings),
        launch: Launch::Settings(pane),
    };
    alloc::vec![
        s("play", "Wave Play", "Entering on connect, sounds, vibration", "play"),
        s("controllers", "Controllers", "Test buttons, sticks and vibration", "controllers"),
        s("display", "Display", "Resolution and scale", "display"),
        s("sound", "Sound", "Output and volume", "sound"),
        s("network", "Network", "Wi-Fi and Ethernet", "network"),
        Entry {
            id: String::from("desktop"),
            title: String::from("Desktop"),
            detail: String::from("Back to your windows"),
            kind: Kind::Setting,
            art: Art::Icon(Icon::Aurora),
            launch: Launch::Desktop,
        },
    ]
}

// ------------------------------------------------------------- recently played

const RECENT_KEY: &str = "play_recent";
const RECENT_MAX: usize = 8;

/// Ids of what was started from Wave Play, most recent first.
pub fn recent() -> Vec<String> {
    crate::gui::settings::get(RECENT_KEY)
        .map(|v| v.split('|').filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

pub fn remember(id: &str) {
    if id == "desktop" || id.starts_with("settings:") {
        return;
    }
    let mut list = recent();
    list.retain(|i| i != id);
    list.insert(0, id.to_string());
    list.truncate(RECENT_MAX);
    crate::gui::settings::set(RECENT_KEY, &list.join("|"));
}

// ------------------------------------------------------------------ artwork

/// Decoded artwork, kept while Wave Play is up.
pub struct Picture {
    pub pixels: Vec<u32>,
    pub w: u32,
    pub h: u32,
    /// The picture's own background, top and bottom, as a 2×2 field
    /// (filtered up, a soft vertical gradient): card backgrounds, the
    /// launch card, the backdrop.
    pub field: [u32; 4],
    /// Its colour lifted for use as a light.
    pub tint: u32,
}

fn decode(path: &str) -> Option<aurora_image::Image> {
    let data = crate::fs::read_all(path).ok()?;
    if data.len() > 8 << 20 {
        return None;
    }
    let img = aurora_image::decode(&data).ok()?;
    Some(if img.width > 640 || img.height > 960 { img.thumbnail(640, 960) } else { img })
}

/// The average opaque colour of the given (x, y) ranges, in thousandths of the size.
fn average(img: &aurora_image::Image, xs: &[(u32, u32)], ys: (u32, u32)) -> Option<u32> {
    let (w, h) = (img.width, img.height);
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for y in h * ys.0 / 1000..h * ys.1 / 1000 {
        for &(x0, x1) in xs {
            for x in w * x0 / 1000..w * x1 / 1000 {
                let p = img.pixels[(y * w + x) as usize];
                if p >> 24 < 220 {
                    continue;
                }
                r += (p >> 16 & 255) as u64;
                g += (p >> 8 & 255) as u64;
                b += (p & 255) as u64;
                n += 1;
            }
        }
    }
    (n > 0).then(|| 0xFF00_0000 | ((r / n) as u32) << 16 | ((g / n) as u32) << 8 | (b / n) as u32)
}

pub fn load(art: &Art) -> Option<Arc<Picture>> {
    let img = match art {
        Art::Icon(i) => match i.picture() {
            Some(name) => decode(&format!("/System/Icons/{name}.png"))?,
            // A drawn icon (no picture): render it once, like a picture.
            None => {
                let mut pixels = alloc::vec![0u32; 256 * 256];
                let mut cv = crate::gui::canvas::Canvas::new(&mut pixels, 256, 256);
                crate::gui::icons::draw(&mut cv, *i, crate::gui::geom::Rect::new(8, 8, 240, 240));
                aurora_image::Image { width: 256, height: 256, pixels }
            }
        },
        Art::Picture(p) => decode(p)?,
    };
    // An icon's background shows beside its symbol, near the top and the
    // bottom of its tile; a picture's in its top and bottom fifths.
    let (top, bottom) = if matches!(art, Art::Icon(_)) {
        let sides = [(140, 300), (700, 860)];
        (average(&img, &sides, (120, 230)), average(&img, &sides, (700, 800)))
    } else {
        (average(&img, &[(0, 1000)], (0, 200)), average(&img, &[(0, 1000)], (800, 1000)))
    };
    let top = top.unwrap_or(0xFF2A_3342);
    let bottom = bottom.unwrap_or(top);
    let tint = lift(crate::gui::canvas::mix(top, bottom, 128));
    let (w, h) = (img.width, img.height);
    Some(Arc::new(Picture { pixels: img.pixels, w, h, field: [top, top, bottom, bottom], tint }))
}

/// A colour made vivid enough to light a dark backdrop: its saturation
/// stretched and its brightest channel brought to about 80%.
fn lift(c: u32) -> u32 {
    let (r, g, b) = ((c >> 16 & 255) as i32, (c >> 8 & 255) as i32, (c & 255) as i32);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max - min < 12 {
        return 0xFF5A_6472;
    }
    let mid = (r + g + b) / 3;
    let stretch = |v: i32| (mid + (v - mid) * 3 / 2).clamp(0, 255);
    let (r, g, b) = (stretch(r), stretch(g), stretch(b));
    let max = r.max(g).max(b).max(1);
    let scale = |v: i32| (v * 205 / max).clamp(0, 255) as u32;
    0xFF00_0000 | scale(r) << 16 | scale(g) << 8 | scale(b)
}

#[cfg(feature = "ktest")]
pub fn parse_for_test() -> bool {
    let d = parse_desktop_file(
        "[Desktop Entry]\nName=Nope\nExec=/nonexistent %U\nCategories=Game;\n[Desktop Action x]\nName=Other\n",
        "nope.desktop",
    );
    let m = parse_manifest("title=Example\nexec=/System/Apps/Clock.elf\nargs=a b\nkind=app\n");
    d.is_none()
        && m.is_some_and(|m| {
            m.kind == Kind::App
                && m.launch
                    == Launch::Program(
                        String::from("/System/Apps/Clock.elf"),
                        alloc::vec![String::from("a"), String::from("b")],
                    )
        })
        && vdf_value("\t\"appid\"\t\t\"620\"\n\t\"name\"\t\t\"Portal 2\"", "name").as_deref() == Some("Portal 2")
}
