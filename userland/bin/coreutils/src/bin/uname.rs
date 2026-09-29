//! uname — system name.
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(_: aurora::Args) -> i32 {
    let i = aurora::process::sys_info();
    aurora::println!("WaveOS Aurora {} Tide x86_64", aurora::process::fixed_str(&i.version, i.version_len));
    0
}
