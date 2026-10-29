//! Stage on the destination volume, flush, publish, then flush again.
//! A post-publication flush error must never be mistaken for an unchanged file.
use alloc::{format, string::String};
use aurora_abi::err;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Published {
    pub sync_error: Option<isize>,
}
impl Published {
    pub fn durable(self) -> bool {
        self.sync_error.is_none()
    }
}

pub trait Storage {
    /// Must create exclusively. Must remove its own partial file on failure, leaving pre-existing files alone.
    fn create_new(&mut self, path: &str, data: &[u8]) -> Result<(), isize>;
    fn sync(&mut self, path: &str) -> Result<(), isize>;
    fn rename(&mut self, from: &str, to: &str) -> Result<(), isize>;
    fn remove(&mut self, path: &str) -> Result<(), isize>;
}

pub fn write<S: Storage>(fs: &mut S, path: &str, data: &[u8], nonce: u64) -> Result<Published, isize> {
    let (parent, name) = path.rsplit_once('/').ok_or(err::EINVAL)?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(err::EINVAL);
    }
    let mut temp = String::new();
    for attempt in 0..128 {
        temp = format!("{parent}/.wave-save-{nonce:x}-{attempt}");
        match fs.create_new(&temp, data) {
            Ok(()) => break,
            Err(e) if e == err::EEXIST => {
                temp.clear();
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    if temp.is_empty() {
        return Err(err::EEXIST);
    }
    if let Err(e) = fs.sync(&temp).and_then(|()| fs.rename(&temp, path)) {
        let _ = fs.remove(&temp);
        return Err(e);
    }
    Ok(Published { sync_error: fs.sync(path).err() })
}

/// Publish a new document name. Update the caller's identity before flushing.
pub fn rename<S: Storage>(fs: &mut S, path: &mut String, to: &str) -> Result<Published, isize> {
    fs.rename(path, to)?;
    *path = String::from(to);
    Ok(Published { sync_error: fs.sync(to).err() })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::{collections::BTreeMap, vec::Vec};
    #[derive(Default)]
    struct Disk {
        files: BTreeMap<String, Vec<u8>>,
        fail: usize,
        step: usize,
        short: bool,
        read_only: bool,
    }
    impl Disk {
        fn tick(&mut self) -> Result<(), isize> {
            self.step += 1;
            if self.step == self.fail {
                Err(err::EIO)
            } else {
                Ok(())
            }
        }
    }
    impl Storage for Disk {
        fn create_new(&mut self, p: &str, d: &[u8]) -> Result<(), isize> {
            if self.read_only {
                return Err(err::EROFS);
            }
            if self.files.contains_key(p) {
                return Err(err::EEXIST);
            }
            self.tick()?;
            self.files.insert(p.into(), if self.short { d[..d.len() / 2].to_vec() } else { d.to_vec() });
            if self.short {
                self.files.remove(p);
                Err(err::ENOSPC)
            } else {
                Ok(())
            }
        }
        fn sync(&mut self, _: &str) -> Result<(), isize> {
            self.tick()
        }
        fn rename(&mut self, a: &str, b: &str) -> Result<(), isize> {
            self.tick()?;
            let d = self.files.remove(a).ok_or(err::ENOENT)?;
            self.files.insert(b.into(), d);
            Ok(())
        }
        fn remove(&mut self, p: &str) -> Result<(), isize> {
            self.files.remove(p);
            Ok(())
        }
    }
    fn disk() -> Disk {
        let mut d = Disk::default();
        d.files.insert("/note".into(), b"original".to_vec());
        d
    }
    #[test]
    fn original_survives_every_prepublication_failure() {
        for fail in 1..=3 {
            let mut d = disk();
            d.fail = fail;
            assert!(write(&mut d, "/note", b"new", 1).is_err());
            assert_eq!(d.files["/note"], b"original");
            assert_eq!(d.files.len(), 1);
        }
    }
    #[test]
    fn read_only_destination_retains_original() {
        let mut d = disk();
        d.read_only = true;
        assert_eq!(write(&mut d, "/note", b"new", 1), Err(err::EROFS));
        assert_eq!(d.files["/note"], b"original");
    }
    #[test]
    fn full_disk_preserves_original() {
        let mut d = disk();
        d.short = true;
        assert_eq!(write(&mut d, "/note", b"new content", 1), Err(err::ENOSPC));
        assert_eq!(d.files["/note"], b"original");
        assert_eq!(d.files.len(), 1);
    }
    #[test]
    fn final_flush_reports_publication() {
        let mut d = disk();
        d.fail = 4;
        let r = write(&mut d, "/note", b"new", 1).unwrap();
        assert_eq!(r.sync_error, Some(err::EIO));
        assert_eq!(d.files["/note"], b"new");
    }
    #[test]
    fn successful_save_and_colliding_temp() {
        let mut d = disk();
        d.files.insert("/.wave-save-1-0".into(), b"someone else".to_vec());
        assert!(write(&mut d, "/note", b"new", 1).unwrap().durable());
        assert_eq!(d.files["/.wave-save-1-0"], b"someone else");
        assert_eq!(d.files["/note"], b"new");
    }
    #[test]
    fn rename_retry_never_resurrects_old_path() {
        let mut d = disk();
        let mut path = String::from("/note");
        d.fail = 1;
        assert!(rename(&mut d, &mut path, "/renamed").is_err());
        assert_eq!(path, "/note");
        d.step = 0;
        d.fail = 2;
        assert_eq!(rename(&mut d, &mut path, "/renamed").unwrap().sync_error, Some(err::EIO));
        assert_eq!(path, "/renamed");
        d.fail = 0;
        assert!(write(&mut d, &path, b"retried", 9).unwrap().durable());
        assert!(!d.files.contains_key("/note"));
        assert_eq!(d.files["/renamed"], b"retried");
    }
}
