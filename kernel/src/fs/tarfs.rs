//! TarFS — the read-only system image. The bootloader loads
//! `\aurora\system.tar` (ustar) and the kernel mounts it at `/System`.

use super::*;
use alloc::collections::BTreeMap;

struct Node {
    kind: Kind,
    data: &'static [u8],
    children: BTreeMap<String, Ino>,
    mtime: u64,
}

pub struct TarFs {
    nodes: Vec<Node>,
}

fn octal(field: &[u8]) -> u64 {
    field.iter().take_while(|&&b| b != 0 && b != b' ').fold(0, |acc, &b| {
        if (b'0'..=b'7').contains(&b) {
            acc * 8 + (b - b'0') as u64
        } else {
            acc
        }
    })
}

fn cstr(field: &[u8]) -> &str {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    core::str::from_utf8(&field[..end]).unwrap_or("")
}

impl TarFs {
    pub fn parse(image: &'static [u8]) -> Option<TarFs> {
        if image.len() < 512 {
            return None;
        }
        let mut fs =
            TarFs { nodes: alloc::vec![Node { kind: Kind::Dir, data: &[], children: BTreeMap::new(), mtime: 0 }] };
        let mut off = 0;
        while off + 512 <= image.len() {
            let h = &image[off..off + 512];
            if h.iter().all(|&b| b == 0) {
                break;
            }
            if &h[257..262] != b"ustar" {
                return None;
            }
            let size = octal(&h[124..136]) as usize;
            let mtime = octal(&h[136..148]);
            let mut name = String::from(cstr(&h[345..500]));
            if !name.is_empty() {
                name.push('/');
            }
            name.push_str(cstr(&h[0..100]));
            let data_start = off + 512;
            let data_end = data_start.checked_add(size)?;
            if data_end > image.len() {
                return None;
            }
            let kind = match h[156] {
                b'0' | 0 => Some(Kind::File),
                b'5' => Some(Kind::Dir),
                _ => None,
            };
            if let Some(kind) = kind {
                let path = name.trim_matches('/');
                // Paths in the archive are relative to the mount point.
                let path =
                    path.strip_prefix("System/").or(if path == "System" { Some("") } else { None }).unwrap_or(path);
                if !path.is_empty() {
                    let ino = fs.ensure(path, kind);
                    if kind == Kind::File {
                        fs.nodes[ino as usize].data = &image[data_start..data_end];
                        fs.nodes[ino as usize].mtime = mtime;
                    }
                }
            }
            off = data_start + size.div_ceil(512) * 512;
        }
        log!("vfs", "system image: {} entries, {} KiB", fs.nodes.len() - 1, image.len() / 1024);
        Some(fs)
    }

    /// Creates (or finds) the node for `path`, creating parent directories.
    fn ensure(&mut self, path: &str, kind: Kind) -> Ino {
        let mut cur = 0usize;
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let next = match self.nodes[cur].children.get(*part) {
                Some(&n) => n as usize,
                None => {
                    let k = if last { kind } else { Kind::Dir };
                    self.nodes.push(Node { kind: k, data: &[], children: BTreeMap::new(), mtime: 0 });
                    let n = self.nodes.len() - 1;
                    self.nodes[cur].children.insert(String::from(*part), n as Ino);
                    n
                }
            };
            cur = next;
        }
        cur as Ino
    }

    fn node(&self, ino: Ino) -> FsResult<&Node> {
        self.nodes.get(ino as usize).ok_or(ENOENT)
    }
}

impl Filesystem for TarFs {
    fn describe(&self) -> String {
        String::from("System image (read-only)")
    }
    fn root(&self) -> Ino {
        0
    }
    fn lookup(&self, dir: Ino, name: &str) -> FsResult<Ino> {
        let d = self.node(dir)?;
        if d.kind != Kind::Dir {
            return Err(ENOTDIR);
        }
        d.children.get(name).copied().ok_or(ENOENT)
    }
    fn metadata(&self, ino: Ino) -> FsResult<Metadata> {
        let n = self.node(ino)?;
        Ok(Metadata { kind: n.kind, size: n.data.len() as u64, mtime: n.mtime, ctime: n.mtime })
    }
    fn read(&self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let n = self.node(ino)?;
        let start = (offset as usize).min(n.data.len());
        let len = buf.len().min(n.data.len() - start);
        buf[..len].copy_from_slice(&n.data[start..start + len]);
        Ok(len)
    }
    fn write(&self, _: Ino, _: u64, _: &[u8]) -> FsResult<usize> {
        Err(EROFS)
    }
    fn truncate(&self, _: Ino, _: u64) -> FsResult<()> {
        Err(EROFS)
    }
    fn readdir(&self, dir: Ino) -> FsResult<Vec<DirEntry>> {
        let d = self.node(dir)?;
        Ok(d.children
            .iter()
            .map(|(name, &ino)| DirEntry { name: name.clone(), ino, kind: self.nodes[ino as usize].kind })
            .collect())
    }
    fn create(&self, _: Ino, _: &str, _: Kind) -> FsResult<Ino> {
        Err(EROFS)
    }
    fn unlink(&self, _: Ino, _: &str) -> FsResult<()> {
        Err(EROFS)
    }
    fn rename(&self, _: Ino, _: &str, _: Ino, _: &str) -> FsResult<()> {
        Err(EROFS)
    }
    fn read_only(&self) -> bool {
        true
    }
}
