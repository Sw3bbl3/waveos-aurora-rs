//! Deliberately crashes (null-pointer write) to exercise fault isolation.

#![no_std]
#![no_main]

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    corekit::println!("crashtest: about to dereference a null pointer…");
    unsafe { core::ptr::write_volatile(core::ptr::null_mut::<u64>(), 42) };
    0
}
