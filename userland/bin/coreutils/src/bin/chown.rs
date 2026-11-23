//! chown OWNER[:GROUP] FILE... — gives files to an account (takes the
//! administrator capability) or, as `:GROUP`, to a group you are in.
#![no_std]
#![no_main]
extern crate alloc;
use corekit::users;
corekit::entry!(main);

fn id(name: &str, group: bool) -> Option<u32> {
    if name.is_empty() {
        return Some(u32::MAX);
    }
    if let Ok(n) = name.parse::<u32>() {
        return Some(n);
    }
    if group {
        match name {
            "system" => return Some(0),
            "admin" => return Some(corekit::abi::cred::ADMIN_GID),
            "users" => return Some(corekit::abi::cred::USERS_GID),
            _ => {}
        }
    } else if name == "system" {
        return Some(0);
    }
    users::by_name(name).map(|u| u.uid)
}

fn main(args: corekit::Args) -> i32 {
    if args.len() < 3 {
        corekit::eprintln!("usage: chown OWNER[:GROUP] FILE...");
        return 2;
    }
    let (owner, group) = args[1].split_once(':').unwrap_or((&args[1], ""));
    let (Some(uid), Some(gid)) = (id(owner, false), id(group, true)) else {
        corekit::eprintln!("chown: {}: no such account or group", args[1]);
        return 1;
    };
    let mut status = 0;
    for f in &args[2..] {
        let p = coreutils::path(f);
        let mut r = users::chown(&p, uid, gid);
        // Not allowed as is: ask an administrator once, then try again.
        if r.as_ref().is_err_and(|e| e.is(corekit::abi::err::EPERM))
            && !users::is_admin()
            && users::elevate("change who owns files").is_ok()
        {
            r = users::chown(&p, uid, gid);
        }
        if let Err(e) = r {
            corekit::eprintln!("chown: {f}: {e}");
            status = 1;
        }
    }
    status
}
