//! record [-d DEVICE] [-t SECONDS] FILE.wav — record sound to a WAV file
//! (48 kHz stereo). `record --list` shows the sound devices.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::vec::Vec;
use corekit::abi::audio as abi;
use corekit::audio::{self, Capture, RATE};
corekit::entry!(main);

fn usage() -> i32 {
    corekit::eprintln!("usage: record [-d DEVICE] [-t SECONDS] FILE.wav | record --list");
    2
}

fn main(args: corekit::Args) -> i32 {
    let mut device = 0u32;
    let mut seconds = 5u32;
    let mut file = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--list" | "-l" => return list(),
            "-d" | "--device" => match args.get(i + 1).and_then(|a| a.parse().ok()) {
                Some(d) => {
                    device = d;
                    i += 1;
                }
                None => return usage(),
            },
            "-t" | "--time" => match args.get(i + 1).and_then(|a| a.parse::<u32>().ok()) {
                Some(s) if (1..=600).contains(&s) => {
                    seconds = s;
                    i += 1;
                }
                _ => return usage(),
            },
            a if file.is_none() && !a.starts_with('-') => file = Some(a),
            _ => return usage(),
        }
        i += 1;
    }
    let Some(file) = file else { return usage() };
    let capture = match Capture::open(device) {
        Ok(c) => c,
        Err(e) if e.0 == corekit::abi::err::ENODEV => {
            corekit::eprintln!("record: no input device");
            return 1;
        }
        Err(e) => {
            corekit::eprintln!("record: {e}");
            return 1;
        }
    };
    let want = (RATE * seconds) as usize * 2;
    let mut samples: Vec<i16> = Vec::with_capacity(want);
    let mut chunk = [0i16; 4800];
    corekit::println!("recording {seconds} s…");
    while samples.len() < want {
        match capture.read(&mut chunk) {
            Ok(0) => {
                corekit::eprintln!("record: the device went away");
                break;
            }
            Ok(n) => samples.extend_from_slice(&chunk[..n.min(want - samples.len())]),
            Err(e) => {
                corekit::eprintln!("record: {e}");
                break;
            }
        }
    }
    drop(capture);
    let path = coreutils::path(file);
    match corekit::fs::write(&path, &audio::encode_wav(&samples)) {
        Ok(()) => {
            corekit::println!("wrote {} ({} ms)", file, samples.len() as u64 / 2 * 1000 / RATE as u64);
            0
        }
        Err(e) => {
            corekit::eprintln!("record: {file}: {e}");
            1
        }
    }
}

/// The sound devices; those in use are marked with `*`.
fn list() -> i32 {
    let all = audio::devices();
    if all.is_empty() {
        corekit::println!("no sound devices");
    }
    for d in all {
        corekit::println!(
            "{} {:>2}  {:<6}  {}  ({} Hz, {} ch, {}-bit{}{})",
            if d.flags & abi::DEFAULT != 0 { '*' } else { ' ' },
            d.id,
            if d.flags & abi::OUTPUT != 0 { "output" } else { "input" },
            d.name(),
            d.rate,
            d.channels,
            d.bits,
            if d.flags & abi::USB != 0 { ", USB" } else { "" },
            if d.flags & abi::HW_VOLUME != 0 { ", device volume" } else { "" }
        );
    }
    0
}
