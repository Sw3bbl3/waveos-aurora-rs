//! ls [-l] [DIR...] — list directory contents (`-l`: mode, owner, group,
//! size and time).
#![no_std]
#![no_main]
extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use corekit::{fs, println, users};
corekit::entry!(main);

/// Account and group names, looked up once each.
struct Names {
    list: alloc::vec::Vec<users::User>,
    groups: BTreeMap<u32, String>,
}

impl Names {
    fn user(&self, uid: u32) -> String {
        match uid {
            0 => String::from("system"),
            _ => self
                .list
                .iter()
                .find(|u| u.uid == uid)
                .map(|u| u.name.clone())
                .unwrap_or_else(|| alloc::format!("{uid}")),
        }
    }
    fn group(&mut self, gid: u32) -> String {
        if let Some(g) = self.groups.get(&gid) {
            return g.clone();
        }
        let name = match gid {
            0 => String::from("system"),
            corekit::abi::cred::ADMIN_GID => String::from("admin"),
            corekit::abi::cred::USERS_GID => String::from("users"),
            g => self.user(g),
        };
        self.groups.insert(gid, name.clone());
        name
    }
}

fn main(args: corekit::Args) -> i32 {
    let long = args.iter().any(|a| a == "-l");
    let mut dirs: alloc::vec::Vec<&str> =
        args[1..].iter().filter(|a| !a.starts_with('-')).map(|s| s.as_str()).collect();
    if dirs.is_empty() {
        dirs.push(".");
    }
    let mut names = Names { list: if long { users::list() } else { alloc::vec::Vec::new() }, groups: BTreeMap::new() };
    let mut status = 0;
    for (i, d) in dirs.iter().enumerate() {
        let p = coreutils::path(d);
        if dirs.len() > 1 {
            println!("{}{}:", if i > 0 { "\n" } else { "" }, p);
        }
        match fs::read_dir(&p) {
            Ok(entries) => {
                for e in entries {
                    if long {
                        let t = corekit::time::from_seconds(e.mtime);
                        let size = if e.is_dir { String::from("-") } else { coreutils::human_size(e.size) };
                        let full = fs::resolve(&p, &e.name);
                        let (mode, owner, group) = match users::owner(&full) {
                            Ok(o) => (users::mode_string(o.mode, e.is_dir), names.user(o.uid), names.group(o.gid)),
                            Err(_) => (
                                String::from(if e.is_dir { "d?????????" } else { "-?????????" }),
                                String::from("?"),
                                String::from("?"),
                            ),
                        };
                        println!(
                            "{} {:<8} {:<8} {:>9}  {} {:>2} {:02}:{:02}  {}{}",
                            mode,
                            owner,
                            group,
                            size,
                            corekit::time::MONTHS[(t.month.clamp(1, 12) - 1) as usize],
                            t.day,
                            t.hour,
                            t.minute,
                            e.name,
                            if e.is_dir { "/" } else { "" }
                        );
                    } else {
                        println!("{}{}", e.name, if e.is_dir { "/" } else { "" });
                    }
                }
            }
            Err(e) => {
                corekit::eprintln!("ls: {d}: {e}");
                status = 1;
            }
        }
    }
    status
}
