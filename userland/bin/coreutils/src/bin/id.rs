//! id [NAME] — user id, group and groups (and the administrator capability).
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use corekit::users;
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let named = args.get(1);
    let (uid, gid, groups, admin) = match named {
        Some(name) => {
            let Some(u) = users::by_name(name) else {
                corekit::eprintln!("id: {name}: no such account");
                return 1;
            };
            let mut g = alloc::vec![corekit::abi::cred::USERS_GID];
            if u.admin {
                g.push(corekit::abi::cred::ADMIN_GID);
            }
            (u.uid, u.uid, g, u.admin)
        }
        None => {
            let c = users::cred();
            let g: Vec<u32> = c.groups[..(c.ngroups as usize).min(16)].to_vec();
            (c.uid, c.gid, g, c.flags & corekit::abi::cred::ADMIN != 0)
        }
    };
    let mut line = alloc::format!(
        "uid={}({}) gid={}({}) groups={}({})",
        uid,
        users::user_name(uid),
        gid,
        users::group_name(gid),
        gid,
        users::group_name(gid)
    );
    for g in groups.iter().filter(|&&g| g != gid) {
        line.push_str(&alloc::format!(",{}({})", g, users::group_name(*g)));
    }
    let cap: String = if admin { String::from(" capabilities=admin") } else { String::new() };
    corekit::println!("{line}{cap}");
    0
}
