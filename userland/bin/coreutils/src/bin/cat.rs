//! cat [FILE...] — print files (or stdin).
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let (inputs, status) = coreutils::inputs(&args[1..], "cat");
    for (_, data) in inputs {
        let _ = aurora::io::write_fd(1, &data);
    }
    status
}
