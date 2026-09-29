//! mv FROM TO — move or rename.
#![no_std]
#![no_main]
extern crate alloc;
use aurora::fs;
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    if args.len() != 3 {
        aurora::eprintln!("usage: mv FROM TO");
        return 2;
    }
    let from = coreutils::path(&args[1]);
    let mut to = coreutils::path(&args[2]);
    if fs::is_dir(&to) {
        to = fs::join(&to, fs::file_name(&from));
    }
    match fs::move_path(&from, &to) {
        Ok(()) => 0,
        Err(e) => {
            aurora::eprintln!("mv: {e}");
            1
        }
    }
}
