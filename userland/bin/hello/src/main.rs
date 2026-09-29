//! The first WaveOS Aurora user program.

#![no_std]
#![no_main]

use aurora::println;

aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    println!("Hello from user space! I am pid {}.", aurora::process::pid());
    if args.len() > 1 {
        println!("You passed: {}", args[1..].join(" "));
    }
    0
}
