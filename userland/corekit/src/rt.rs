//! Program start-up and shutdown.

use alloc::string::String;
use alloc::vec::Vec;

/// Command-line arguments (`args[0]` is the program path).
pub type Args = Vec<String>;

pub fn start(block: *const u64, main: fn(Args) -> i32) -> ! {
    let args = unsafe { parse_args(block) };
    let code = main(args);
    crate::process::exit(code)
}

unsafe fn parse_args(block: *const u64) -> Args {
    let mut out = Vec::new();
    if block.is_null() {
        return out;
    }
    let argc = *block as usize;
    for i in 0..argc.min(256) {
        let p = *block.add(1 + i) as *const u8;
        let mut len = 0;
        while *p.add(len) != 0 && len < 4096 {
            len += 1;
        }
        let bytes = core::slice::from_raw_parts(p, len);
        out.push(String::from_utf8_lossy(bytes).into_owned());
    }
    out
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    let _ = writeln!(crate::io::Stderr, "panic: {}", info);
    crate::process::exit(101)
}
