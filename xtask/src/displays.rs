//! `cargo xtask run --displays N`: several simulated monitors, each in a
//! window of its own on the host.
//!
//! The guest side: with `--gpu virtio`, one virtio-vga with `max_outputs=N`
//! (N scanouts, each with its own EDID; the Linux driver logs "number of
//! scanouts: N"); with `--gpu std`, the usual VGA plus a `secondary-vga` per
//! extra display, each with its own EDID and preferred size.
//!
//! The host side, which depends on what QEMU was built with:
//! - SDL (most Linux builds): `-display sdl` opens one window per display.
//! - macOS: QEMU's Cocoa UI has a single window that shows one display at a
//!   time (View menu). Display 1 stays there; every other display gets its
//!   own VNC server on 127.0.0.1 and opens in a Screen Sharing window
//!   (which needs a VNC password: a random one per run, passed in the URL).
//! - Otherwise (GTK only): display 1 in QEMU's window, the others on VNC,
//!   opened with `remote-viewer` or `vncviewer` when one is installed.
//!
//! With `--gpu virtio` WaveOS extends its desktop over every display (see
//! docs/developer/display-core.md); with `--gpu std` it drives the first
//! VGA only, so the `secondary-vga` windows stay blank.

use crate::runconfig::{shell_quote, QemuCaps};
use std::hash::{BuildHasher, Hasher};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

pub const MAX_DISPLAYS: usize = 4;
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);

pub fn check_size(w: u32, h: u32) -> Result<(), String> {
    if (640..=8192).contains(&w) && (480..=8192).contains(&h) {
        Ok(())
    } else {
        Err(format!("{w}x{h}: a display is 640x480 to 8192x8192"))
    }
}

/// "1920x1080" → (1920, 1080), if it's a size we can show.
pub fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.trim().split_once('x')?;
    let size = (w.parse().ok()?, h.parse().ok()?);
    check_size(size.0, size.1).ok().map(|_| size)
}

/// The display sizes `--displays N` and `--resolution WxH[,WxH…]` ask for:
/// one size for all, or one per display (the list's length is then the
/// number of displays if `--displays` isn't given).
pub fn parse_sizes(args: &[String]) -> Result<Vec<(u32, u32)>, String> {
    let value = |name: &str| args.iter().position(|a| a == name).map(|i| args.get(i + 1).cloned().unwrap_or_default());
    let count = match value("--displays") {
        None => None,
        Some(n) => match n.parse::<usize>() {
            Ok(n) if (1..=MAX_DISPLAYS).contains(&n) => Some(n),
            _ => return Err(format!("--displays must be between 1 and {MAX_DISPLAYS}")),
        },
    };
    let sizes = match value("--resolution") {
        None => Vec::new(),
        Some(list) => list
            .split(',')
            .map(|s| {
                parse_size(s).ok_or_else(|| {
                    format!("--resolution must be WIDTHxHEIGHT[,WIDTHxHEIGHT…], e.g. 2560x1600, not {s:?}")
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    match (count, sizes.len()) {
        (None, 0) => Ok(vec![DEFAULT_SIZE]),
        (Some(n), 0) => Ok(vec![DEFAULT_SIZE; n]),
        (Some(n), 1) => Ok(vec![sizes[0]; n]),
        (None, k) if k <= MAX_DISPLAYS => Ok(sizes),
        (Some(n), k) if k == n => Ok(sizes),
        (_, k) => Err(format!("--resolution lists {k} sizes for {} displays", count.unwrap_or(k))),
    }
}

/// Like [`parse_sizes`], for `main`: a message and exit status 2 on errors.
pub fn from_args(args: &[String], headless: bool) -> Displays {
    match parse_sizes(args) {
        Ok(sizes) => Displays::new(sizes, headless),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}

/// A VNC server for one display.
#[derive(Clone, Debug)]
pub struct Vnc {
    /// Display index (1 = the second display).
    pub index: usize,
    pub port: u16,
    /// Set when the viewer needs one (macOS Screen Sharing does).
    pub password: Option<String>,
}

/// How the displays reach the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Windows {
    /// One display: QEMU's own window, as always.
    Single,
    /// No windows at all (`--headless`); the guest still sees every display.
    Headless,
    /// SDL: QEMU opens a window per display.
    Sdl,
    /// Display 1 in QEMU's window; the others on VNC servers with viewer windows.
    Vnc,
    /// No way to show the extra displays (no SDL, no VNC): QEMU's own window
    /// lists them (View menu on macOS, tabs in GTK).
    QemuMenu,
}

#[derive(Clone, Debug)]
pub struct Displays {
    pub sizes: Vec<(u32, u32)>,
    pub windows: Windows,
    pub vnc: Vec<Vnc>,
}

impl Displays {
    /// One display at `size`, as `cargo xtask test` uses.
    pub fn single(size: (u32, u32)) -> Displays {
        Displays { sizes: vec![size], windows: Windows::Single, vnc: Vec::new() }
    }

    /// Decides how to show `sizes.len()` displays with the local QEMU.
    pub fn new(sizes: Vec<(u32, u32)>, headless: bool) -> Displays {
        let caps = if sizes.len() > 1 && !headless { QemuCaps::probe() } else { QemuCaps::default() };
        Displays::plan(sizes, headless, cfg!(target_os = "macos"), &caps)
    }

    /// [`Displays::new`] for a given host (separate for the tests).
    pub fn plan(sizes: Vec<(u32, u32)>, headless: bool, macos: bool, caps: &QemuCaps) -> Displays {
        let windows = if sizes.len() == 1 {
            Windows::Single
        } else if headless {
            Windows::Headless
        } else if !macos && caps.displays.contains("sdl") {
            Windows::Sdl
        } else if caps.vnc {
            Windows::Vnc
        } else {
            Windows::QemuMenu
        };
        let mut vnc = Vec::new();
        if windows == Windows::Vnc {
            let mut next = 5901;
            for index in 1..sizes.len() {
                let port = (next..5999).find(|&p| TcpListener::bind(("127.0.0.1", p)).is_ok()).unwrap_or(next);
                next = port + 1;
                vnc.push(Vnc { index, port, password: macos.then(password) });
            }
        }
        Displays { sizes, windows, vnc }
    }

    /// Display 1's size.
    pub fn primary(&self) -> (u32, u32) {
        self.sizes[0]
    }

    /// Extra properties for the virtio-vga device.
    pub fn virtio_props(&self) -> String {
        if self.sizes.len() > 1 {
            format!(",id=video0,max_outputs={}", self.sizes.len())
        } else {
            String::new()
        }
    }

    /// The QEMU console (device and head) that shows display `index`.
    fn console(&self, gpu: &str, index: usize) -> String {
        if gpu.starts_with("virtio") {
            format!("display=video0,head={index}")
        } else {
            format!("display=video{index}")
        }
    }

    /// QEMU arguments for displays 2 to N (nothing for one display).
    pub fn qemu_args(&self, gpu: &str) -> Vec<String> {
        let mut a = Vec::new();
        if !gpu.starts_with("virtio") {
            // virtio-vga has its outputs built in; the Bochs VGA needs a
            // second device per display, each with its own EDID.
            for (i, &(w, h)) in self.sizes.iter().enumerate().skip(1) {
                a.push("-device".into());
                a.push(format!("secondary-vga,id=video{i},xres={w},yres={h},vgamem_mb=64"));
            }
        }
        for v in &self.vnc {
            let mut spec = format!("127.0.0.1:{},id=vnc{},{}", v.port - 5900, v.index, self.console(gpu, v.index));
            if let Some(pw) = &v.password {
                a.push("-object".into());
                a.push(format!("secret,id=vncpw{},data={pw}", v.index));
                spec.push_str(&format!(",password-secret=vncpw{}", v.index));
            }
            a.push("-vnc".into());
            a.push(spec);
        }
        if self.windows == Windows::Sdl {
            a.push("-display".into());
            a.push("sdl".into());
        }
        a
    }

    /// Whether these arguments choose the display backend themselves.
    pub fn chooses_backend(&self) -> bool {
        self.windows == Windows::Sdl
    }

    /// Where each display shows up, one line each, for the terminal and the launcher.
    pub fn describe(&self) -> Vec<String> {
        let n = self.sizes.len();
        let mut out = Vec::new();
        for (i, &(w, h)) in self.sizes.iter().enumerate() {
            let place = match (&self.windows, i) {
                (Windows::Single, _) => "the QEMU window".to_string(),
                (Windows::Headless, _) => "no window (--headless)".to_string(),
                (Windows::Sdl, _) => format!("SDL window {}", i + 1),
                (Windows::Vnc | Windows::QemuMenu, 0) => "the QEMU window".to_string(),
                (Windows::Vnc, _) => {
                    let v = &self.vnc[i - 1];
                    let viewer = if v.password.is_some() { "a Screen Sharing window" } else { "a VNC viewer" };
                    format!("{viewer}, vnc://127.0.0.1:{}", v.port)
                }
                (Windows::QemuMenu, _) => "QEMU's View menu (no SDL or VNC in this QEMU)".to_string(),
            };
            out.push(format!("display {} of {n} ({w}x{h}): {place}", i + 1));
        }
        if n > 1 {
            out.push(String::from(
                "With --gpu virtio WaveOS extends its desktop over every display (arrange them in Settings → \
                 Display); with --gpu std only display 1 is used.",
            ));
        }
        out
    }

    /// Called just before QEMU starts: prints the command (passwords hidden)
    /// and where each display is, and opens the viewer windows once their
    /// VNC servers are listening.
    pub fn starting(&self, cmd: &Command) {
        eprintln!("starting QEMU: {}", printable(cmd));
        if self.sizes.len() > 1 {
            for line in self.describe() {
                eprintln!("  {line}");
            }
        }
        for v in self.vnc.clone() {
            std::thread::spawn(move || open_viewer(&v));
        }
    }
}

/// The command line as text, with VNC passwords left out.
pub fn printable(cmd: &Command) -> String {
    let mut words = vec![cmd.get_program().to_string_lossy().into_owned()];
    for a in cmd.get_args() {
        let a = a.to_string_lossy();
        let a = match a.split_once(",data=") {
            Some((head, _)) if a.starts_with("secret,") => format!("{head},data=…"),
            _ => a.into_owned(),
        };
        words.push(shell_quote(&a));
    }
    words.join(" ")
}

/// Eight letters and digits (a VNC password holds at most eight).
fn password() -> String {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(Instant::now().elapsed().as_nanos() ^ std::process::id() as u128);
    let mut n = h.finish();
    let alphabet = b"abcdefghijkmnpqrstuvwxyz23456789";
    (0..8)
        .map(|_| {
            let c = alphabet[(n % alphabet.len() as u64) as usize] as char;
            n /= alphabet.len() as u64;
            c
        })
        .collect()
}

/// Waits for QEMU's VNC server, then opens a viewer window on it.
fn open_viewer(v: &Vnc) {
    let start = Instant::now();
    while TcpStream::connect(("127.0.0.1", v.port)).is_err() {
        if start.elapsed() > Duration::from_secs(60) {
            eprintln!("display {}: QEMU's VNC server on port {} never came up", v.index + 1, v.port);
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let opened = if cfg!(target_os = "macos") {
        let url = match &v.password {
            Some(pw) => format!("vnc://:{pw}@127.0.0.1:{}", v.port),
            None => format!("vnc://127.0.0.1:{}", v.port),
        };
        Command::new("open").arg(url).status().is_ok_and(|s| s.success())
    } else {
        let address = format!("127.0.0.1::{}", v.port);
        let url = format!("vnc://127.0.0.1:{}", v.port);
        [("remote-viewer", url.as_str()), ("vncviewer", address.as_str()), ("gvncviewer", address.as_str())]
            .iter()
            .any(|(viewer, target)| Command::new(viewer).arg(target).spawn().is_ok())
    };
    if !opened {
        eprintln!(
            "display {}: open vnc://127.0.0.1:{} in a VNC viewer (install remote-viewer or TigerVNC's vncviewer \
             to have it open by itself)",
            v.index + 1,
            v.port
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn sizes_from_the_command_line() {
        assert_eq!(parse_sizes(&args("")).unwrap(), vec![DEFAULT_SIZE]);
        assert_eq!(parse_sizes(&args("--displays 3")).unwrap(), vec![DEFAULT_SIZE; 3]);
        assert_eq!(parse_sizes(&args("--resolution 2560x1600")).unwrap(), vec![(2560, 1600)]);
        assert_eq!(parse_sizes(&args("--displays 2 --resolution 1280x800")).unwrap(), vec![(1280, 800); 2]);
        assert_eq!(parse_sizes(&args("--resolution 1920x1080,1280x800")).unwrap(), vec![(1920, 1080), (1280, 800)]);
        assert!(parse_sizes(&args("--displays 5")).is_err());
        assert!(parse_sizes(&args("--displays 0")).is_err());
        assert!(parse_sizes(&args("--displays 3 --resolution 1920x1080,1280x800")).is_err());
        assert!(parse_sizes(&args("--resolution 100x100")).is_err());
    }

    fn caps(displays: &[&str], vnc: bool) -> QemuCaps {
        QemuCaps { found: true, displays: displays.iter().map(|s| s.to_string()).collect(), vnc, ..Default::default() }
    }

    #[test]
    fn one_display_changes_nothing() {
        let d = Displays::plan(vec![DEFAULT_SIZE], false, true, &caps(&["cocoa"], true));
        assert_eq!(d.windows, Windows::Single);
        assert!(d.qemu_args("std").is_empty() && d.qemu_args("virtio").is_empty());
        assert_eq!(d.virtio_props(), "");
    }

    #[test]
    fn macos_puts_extra_displays_on_vnc_with_a_password() {
        let d = Displays::plan(vec![(1920, 1080), (1280, 800)], false, true, &caps(&["cocoa", "none"], true));
        assert_eq!(d.windows, Windows::Vnc);
        assert_eq!(d.vnc.len(), 1);
        let pw = d.vnc[0].password.clone().unwrap();
        assert_eq!(pw.len(), 8);
        let port = d.vnc[0].port;
        let std = d.qemu_args("std").join(" ");
        assert_eq!(
            std,
            format!(
                "-device secondary-vga,id=video1,xres=1280,yres=800,vgamem_mb=64 -object secret,id=vncpw1,data={pw} \
                 -vnc 127.0.0.1:{},id=vnc1,display=video1,password-secret=vncpw1",
                port - 5900
            )
        );
        let virtio = d.qemu_args("virtio").join(" ");
        assert!(virtio.contains("display=video0,head=1") && !virtio.contains("secondary-vga"));
        assert_eq!(d.virtio_props(), ",id=video0,max_outputs=2");
        let mut cmd = Command::new("qemu-system-x86_64");
        cmd.args(d.qemu_args("std"));
        assert!(!printable(&cmd).contains(&pw));
    }

    #[test]
    fn linux_prefers_sdl_windows() {
        let d = Displays::plan(vec![DEFAULT_SIZE; 3], false, false, &caps(&["gtk", "sdl"], true));
        assert_eq!(d.windows, Windows::Sdl);
        assert!(d.vnc.is_empty() && d.chooses_backend());
        assert!(d.qemu_args("virtio").ends_with(&["-display".to_string(), "sdl".to_string()]));
        let gtk = Displays::plan(vec![DEFAULT_SIZE; 2], false, false, &caps(&["gtk"], true));
        assert_eq!(gtk.windows, Windows::Vnc);
        assert!(gtk.vnc[0].password.is_none());
    }

    #[test]
    fn headless_keeps_the_displays_without_windows() {
        let d = Displays::plan(vec![DEFAULT_SIZE; 2], true, true, &caps(&["cocoa"], true));
        assert_eq!(d.windows, Windows::Headless);
        assert_eq!(d.qemu_args("std"), ["-device", "secondary-vga,id=video1,xres=1920,yres=1080,vgamem_mb=64"]);
        let none = Displays::plan(vec![DEFAULT_SIZE; 2], false, true, &caps(&["cocoa"], false));
        assert_eq!(none.windows, Windows::QemuMenu);
        assert!(none.describe().iter().any(|l| l.contains("View menu")));
    }
}
