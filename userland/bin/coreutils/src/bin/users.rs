//! users [add NAME [FULL NAME] [--admin] | remove NAME | admin NAME on|off]
//! — lists the accounts, or manages them (administrators; others get the
//! system prompt).
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use corekit::users;
corekit::entry!(main);

fn read_secret(prompt: &str) -> String {
    corekit::print!("{prompt}");
    let mut line = alloc::vec::Vec::new();
    let mut b = [0u8; 1];
    while let Ok(1) = corekit::io::read_fd(0, &mut b) {
        if b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
    }
    String::from_utf8_lossy(&line).into_owned()
}

fn main(args: corekit::Args) -> i32 {
    let words: alloc::vec::Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    let result = match words.as_slice() {
        [] => {
            for u in users::list() {
                corekit::println!(
                    "{:>5}  {:<12} {:<24} {}{}{}",
                    u.uid,
                    u.name,
                    u.full,
                    u.home,
                    if u.admin { "  admin" } else { "" },
                    if u.current { "  (logged in)" } else { "" }
                );
            }
            return 0;
        }
        ["add", name, rest @ ..] => {
            let admin = rest.contains(&"--admin");
            let full: alloc::vec::Vec<&str> = rest.iter().copied().filter(|w| *w != "--admin").collect();
            corekit::println!("Leave the password empty for an account without one.");
            let password = read_secret("New password:");
            let again = read_secret("Retype new password:");
            if password != again {
                corekit::eprintln!("users: the passwords don't match");
                return 1;
            }
            users::add(name, &full.join(" "), &password, admin).map(|uid| corekit::println!("added {name} (uid {uid})"))
        }
        ["remove", name] => match users::by_name(name) {
            Some(u) => users::remove(u.uid),
            None => Err(corekit::Error(-aurora_abi_err::ENOENT)),
        },
        ["admin", name, on] => match users::by_name(name) {
            Some(u) => users::set_admin(u.uid, matches!(*on, "on" | "yes" | "1")),
            None => Err(corekit::Error(-aurora_abi_err::ENOENT)),
        },
        _ => {
            corekit::eprintln!("usage: users [add NAME [FULL NAME] [--admin] | remove NAME | admin NAME on|off]");
            return 2;
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            corekit::eprintln!("users: {e}");
            1
        }
    }
}

use corekit::abi::err as aurora_abi_err;
