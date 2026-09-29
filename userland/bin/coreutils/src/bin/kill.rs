//! kill PID... — terminate processes.
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let mut status = 0;
    for a in &args[1..] {
        match a.parse::<u32>() {
            Ok(pid) => {
                if let Err(e) = aurora::process::kill(pid) {
                    aurora::eprintln!("kill: {pid}: {e}");
                    status = 1;
                }
            }
            Err(_) => {
                aurora::eprintln!("kill: {a}: not a pid");
                status = 2;
            }
        }
    }
    status
}
