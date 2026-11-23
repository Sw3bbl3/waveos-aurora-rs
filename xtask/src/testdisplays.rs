//! `cargo xtask test --displays N`: the harness side of the multi-monitor
//! kernel test.
//!
//! QEMU gets one virtio-vga with N scanouts. Scanout 0 is on from the
//! start; QEMU switches the others on only when a user interface asks for a
//! size, so each extra head gets a VNC server on a Unix socket, and the
//! harness plays the viewer: `SetDesktopSize` with a size plugs the display
//! in, with 0×0 it unplugs it (QEMU's `ui_info`, which raises the guest's
//! display-change event either way).
//!
//! The guest drives it with serial lines:
//! - `harness: display set HEAD WxH` (0x0: unplug);
//! - `harness: screendump HEAD X,Y=RRGGBB …`: the harness takes a QMP
//!   `screendump` of that head and checks each pixel is what the guest's
//!   compositor drew there, so what each scanout really shows is verified.
//!   A mismatch fails the run.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The VNC socket of head `head` in the session directory.
pub fn vnc_socket(dir: &Path, head: usize) -> PathBuf {
    dir.join(format!("vnc-head{head}.sock"))
}

/// QEMU arguments: a VNC server on a Unix socket for every head but the first.
pub fn qemu_args(dir: &Path, heads: usize) -> Vec<String> {
    let mut a = Vec::new();
    for head in 1..heads {
        let sock = vnc_socket(dir, head);
        let _ = std::fs::remove_file(&sock);
        a.push("-vnc".into());
        a.push(format!("unix:{},id=vnc{head},display=video0,head={head}", sock.display()));
    }
    a
}

/// A minimal RFB 3.8 viewer: no password, shared, asks for nothing but a size.
fn vnc_connect(path: &Path) -> std::io::Result<UnixStream> {
    let mut s = UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut version = [0u8; 12];
    s.read_exact(&mut version)?;
    s.write_all(b"RFB 003.008\n")?;
    let mut n = [0u8; 1];
    s.read_exact(&mut n)?;
    let mut types = vec![0u8; n[0] as usize];
    s.read_exact(&mut types)?;
    if !types.contains(&1) {
        return Err(std::io::Error::other("the VNC server wants a password"));
    }
    s.write_all(&[1])?;
    let mut result = [0u8; 4];
    s.read_exact(&mut result)?;
    if result != [0; 4] {
        return Err(std::io::Error::other("VNC security handshake failed"));
    }
    s.write_all(&[1])?; // shared
    let mut init = [0u8; 24];
    s.read_exact(&mut init)?;
    let mut name = vec![0u8; u32::from_be_bytes([init[20], init[21], init[22], init[23]]) as usize];
    s.read_exact(&mut name)?;
    // SetEncodings: raw, DesktopSize, ExtendedDesktopSize.
    let mut msg = vec![2, 0, 0, 3];
    for e in [0i32, -223, -308] {
        msg.extend_from_slice(&e.to_be_bytes());
    }
    s.write_all(&msg)?;
    Ok(s)
}

/// SetDesktopSize: one screen of `w`×`h` (0×0 switches the head off).
fn vnc_set_size(s: &mut UnixStream, w: u16, h: u16) -> std::io::Result<()> {
    let mut msg = vec![251, 0];
    msg.extend_from_slice(&w.to_be_bytes());
    msg.extend_from_slice(&h.to_be_bytes());
    msg.extend_from_slice(&[1, 0]);
    msg.extend_from_slice(&0u32.to_be_bytes());
    msg.extend_from_slice(&[0, 0, 0, 0]);
    msg.extend_from_slice(&w.to_be_bytes());
    msg.extend_from_slice(&h.to_be_bytes());
    msg.extend_from_slice(&0u32.to_be_bytes());
    s.write_all(&msg)
}

/// The harness's viewers, one per head, kept open for the whole run.
#[derive(Default)]
pub struct Harness {
    viewers: BTreeMap<usize, UnixStream>,
    /// Several displays: screendumps name the device and head.
    pub multi: bool,
    pub failed: bool,
}

impl Harness {
    pub fn new(multi: bool) -> Harness {
        Harness { multi, ..Default::default() }
    }

    /// Handles a `harness: display …` or `harness: screendump …` line.
    pub fn line(&mut self, dir: &Path, line: &str, qmp: impl Fn(&str) -> std::io::Result<String>) {
        if let Some(rest) = line.split("harness: display set ").nth(1) {
            let mut f = rest.split_whitespace();
            let head: usize = f.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let (w, h) = f
                .next()
                .and_then(|v| v.split_once('x'))
                .and_then(|(w, h)| Some((w.parse::<u16>().ok()?, h.parse::<u16>().ok()?)))
                .unwrap_or((0, 0));
            if let Err(e) = self.set_size(dir, head, w, h) {
                println!("harness: display {head} could not be set to {w}x{h}: {e}");
                self.failed = true;
            } else {
                println!("harness: display {head} set to {w}x{h}");
            }
        } else if let Some(rest) = line.split("harness: screendump ").nth(1) {
            self.screendump(dir, rest, qmp);
        }
    }

    fn set_size(&mut self, dir: &Path, head: usize, w: u16, h: u16) -> std::io::Result<()> {
        if !self.viewers.contains_key(&head) {
            let s = vnc_connect(&vnc_socket(dir, head))?;
            self.viewers.insert(head, s);
        }
        vnc_set_size(self.viewers.get_mut(&head).unwrap(), w, h)
    }

    fn screendump(&mut self, dir: &Path, rest: &str, qmp: impl Fn(&str) -> std::io::Result<String>) {
        let mut f = rest.split_whitespace();
        let head: usize = f.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let file = dir.join(format!("screendump-head{head}.ppm"));
        let _ = std::fs::remove_file(&file);
        let target = if self.multi { format!(r#", "device": "video0", "head": {head}"#) } else { String::new() };
        let cmd = format!(r#"{{"execute": "screendump", "arguments": {{"filename": "{}"{target}}}}}"#, file.display());
        let picture = qmp(&cmd).and_then(|_| std::fs::read(&file)).map_err(|e| e.to_string()).and_then(|d| ppm(&d));
        let (w, h, rgb) = match picture {
            Ok(p) => p,
            Err(e) => {
                println!("harness: screendump {head} FAILED: {e}");
                self.failed = true;
                return;
            }
        };
        let mut checked = 0;
        for sample in f {
            let Some((at, want)) = sample.split_once('=') else { continue };
            let Some((x, y)) = at.split_once(',').and_then(|(x, y)| Some((x.parse::<u32>().ok()?, y.parse::<u32>().ok()?)))
            else {
                continue;
            };
            let want = u32::from_str_radix(want, 16).unwrap_or(0);
            if x >= w || y >= h {
                println!("harness: screendump {head} FAILED: ({x}, {y}) is outside the {w}x{h} picture");
                self.failed = true;
                return;
            }
            let i = ((y * w + x) * 3) as usize;
            let got = (rgb[i] as u32) << 16 | (rgb[i + 1] as u32) << 8 | rgb[i + 2] as u32;
            if got != want {
                println!("harness: screendump {head} FAILED at ({x}, {y}): shows {got:06x}, drawn {want:06x}");
                self.failed = true;
                return;
            }
            checked += 1;
        }
        println!("harness: screendump {head} ({w}x{h}) matches the compositor at {checked} points");
    }
}

/// A binary PPM (as QEMU's screendump writes it): width, height, RGB bytes.
fn ppm(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 && i < data.len() {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < data.len() && !data[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&data[start..i]).into_owned());
    }
    if fields.len() < 4 || fields[0] != "P6" || fields[3] != "255" {
        return Err("not a binary PPM".into());
    }
    let (w, h): (u32, u32) = (fields[1].parse().map_err(|_| "bad width")?, fields[2].parse().map_err(|_| "bad height")?);
    let body = &data[i + 1..];
    if body.len() < (w * h * 3) as usize {
        return Err("short PPM".into());
    }
    Ok((w, h, body.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_qemu_screendumps() {
        let mut d = b"P6\n2 1\n255\n".to_vec();
        d.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let (w, h, px) = ppm(&d).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, [1, 2, 3, 4, 5, 6]);
        assert!(ppm(b"P5\n1 1\n255\n\0").is_err());
    }

    #[test]
    fn a_vnc_server_per_extra_head() {
        let a = qemu_args(Path::new("/tmp/s"), 3).join(" ");
        assert_eq!(
            a,
            "-vnc unix:/tmp/s/vnc-head1.sock,id=vnc1,display=video0,head=1 \
             -vnc unix:/tmp/s/vnc-head2.sock,id=vnc2,display=video0,head=2"
        );
        assert!(qemu_args(Path::new("/tmp/s"), 1).is_empty());
    }
}
