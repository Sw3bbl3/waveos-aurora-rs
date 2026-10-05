//! cp FROM TO — copy files or directory trees.
#![no_std]
#![no_main]
extern crate alloc;
use corekit::fs;
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let a: alloc::vec::Vec<&alloc::string::String> = args[1..].iter().filter(|a| !a.starts_with('-')).collect();
    if a.len() != 2 {
        corekit::eprintln!("usage: cp FROM TO");
        return 2;
    }
    let from = coreutils::path(a[0]);
    let mut to = coreutils::path(a[1]);
    if fs::is_dir(&to) {
        to = fs::join(&to, fs::file_name(&from));
    }
    match fs::copy(&from, &to) {
        Ok(()) => 0,
        Err(e) => {
            corekit::eprintln!("cp: {e}");
            1
        }
    }
}
