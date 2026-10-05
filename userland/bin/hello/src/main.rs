//! The first WaveOS Aurora user program.

#![no_std]
#![no_main]

use corekit::println;

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    println!("Hello from user space! I am pid {}.", corekit::process::pid());
    if args.len() > 1 {
        println!("You passed: {}", args[1..].join(" "));
    }
    0
}
