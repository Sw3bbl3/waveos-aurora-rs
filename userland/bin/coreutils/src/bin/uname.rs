//! uname — system name.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    let i = corekit::process::sys_info();
    corekit::println!(
        "WaveOS Aurora {} Aster {}",
        corekit::process::fixed_str(&i.version, i.version_len),
        corekit::abi::ARCH
    );
    0
}
