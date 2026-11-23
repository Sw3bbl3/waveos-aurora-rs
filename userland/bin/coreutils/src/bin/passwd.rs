//! passwd [NAME] — changes a password: your own (asks for the current one
//! first), or another account's (takes an administrator's approval).
//! An empty new password removes it.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use corekit::users;
corekit::entry!(main);

/// Reads one line from stdin. The Terminal shows what is typed after a
/// prompt ending in "password:" as dots and doesn't echo it.
fn read_secret(prompt: &str) -> Option<String> {
    corekit::print!("{prompt}");
    let mut line = alloc::vec::Vec::new();
    let mut b = [0u8; 1];
    loop {
        match corekit::io::read_fd(0, &mut b) {
            Ok(1) if b[0] == b'\n' => break,
            Ok(1) => line.push(b[0]),
            _ if line.is_empty() => return None,
            _ => break,
        }
    }
    Some(String::from_utf8_lossy(&line).into_owned())
}

fn main(args: corekit::Args) -> i32 {
    let me = users::uid();
    let target = match args.get(1) {
        Some(name) => match users::by_name(name) {
            Some(u) => u,
            None => {
                corekit::eprintln!("passwd: {name}: no such account");
                return 1;
            }
        },
        None => match users::by_uid(me) {
            Some(u) => u,
            None => {
                corekit::eprintln!("passwd: this process has no account");
                return 1;
            }
        },
    };
    corekit::println!("Changing the password of {}.", target.name);
    let old = if target.uid == me && target.has_password {
        match read_secret("Current password:") {
            Some(p) => p,
            None => return 1,
        }
    } else {
        String::new()
    };
    let Some(new) = read_secret("New password:") else { return 1 };
    let Some(again) = read_secret("Retype new password:") else { return 1 };
    if new != again {
        corekit::eprintln!("passwd: the passwords don't match");
        return 1;
    }
    if !new.is_empty() && new.chars().count() < 4 {
        corekit::eprintln!("passwd: use at least 4 characters (or none)");
        return 1;
    }
    match users::set_password(target.uid, &old, &new) {
        Ok(()) => {
            corekit::println!("{}", if new.is_empty() { "Password removed." } else { "Password changed." });
            0
        }
        Err(e) if e.is(corekit::abi::err::EACCES) => {
            corekit::eprintln!("passwd: that isn't the current password");
            1
        }
        Err(e) if e.is(corekit::abi::err::EAGAIN) => {
            corekit::eprintln!("passwd: too many tries; wait a little");
            1
        }
        Err(e) => {
            corekit::eprintln!("passwd: {e}");
            1
        }
    }
}
