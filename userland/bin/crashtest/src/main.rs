//! Deliberately crashes (null-pointer write) to exercise fault isolation.

#![no_std]
#![no_main]

aurora::entry!(main);

fn main(_: aurora::Args) -> i32 {
    aurora::println!("crashtest: about to dereference a null pointer…");
    unsafe { core::ptr::write_volatile(core::ptr::null_mut::<u64>(), 42) };
    0
}
