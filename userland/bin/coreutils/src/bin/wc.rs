//! wc [FILE...] — count lines, words and bytes.
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let (inputs, status) = coreutils::inputs(&args[1..], "wc");
    let (mut tl, mut tw, mut tb) = (0, 0, 0);
    let many = inputs.len() > 1;
    for (name, data) in &inputs {
        let text = alloc::string::String::from_utf8_lossy(data);
        let (l, w, b) = (text.matches('\n').count(), text.split_whitespace().count(), data.len());
        tl += l;
        tw += w;
        tb += b;
        aurora::println!("{l:>7} {w:>7} {b:>7} {}", if name == "-" { "" } else { name });
    }
    if many {
        aurora::println!("{tl:>7} {tw:>7} {tb:>7} total");
    }
    status
}
