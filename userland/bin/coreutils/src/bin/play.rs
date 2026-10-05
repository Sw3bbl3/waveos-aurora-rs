//! play FILE.wav... — play sound files. `play --tone HZ [MS]` plays a tone.
#![no_std]
#![no_main]
extern crate alloc;
use corekit::audio::{self, Stream, RATE};
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    if args.len() < 2 {
        corekit::eprintln!("usage: play FILE.wav... | play --tone HZ [MS]");
        return 2;
    }
    if args[1] == "--tone" {
        let hz = args.get(2).and_then(|a| a.parse::<u32>().ok()).unwrap_or(440).clamp(20, 20_000);
        let ms = args.get(3).and_then(|a| a.parse::<u32>().ok()).unwrap_or(500).min(60_000);
        return tone(hz, ms);
    }
    let mut status = 0;
    for a in &args[1..] {
        let path = coreutils::path(a);
        let result = match corekit::fs::read(&path) {
            Ok(data) => match audio::decode_wav(&data) {
                Some(samples) => Stream::open().and_then(|mut s| s.write(&samples)),
                None => {
                    corekit::eprintln!("play: {a}: not a PCM WAV file");
                    status = 1;
                    continue;
                }
            },
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            corekit::eprintln!("play: {a}: {e}");
            status = 1;
        }
    }
    status
}

/// A sine tone with 5 ms fades, so it starts and stops without a click.
fn tone(hz: u32, ms: u32) -> i32 {
    let frames = (RATE * ms / 1000) as usize;
    let fade = (RATE / 200) as usize;
    let mut samples = alloc::vec::Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let t = i as f32 / RATE as f32;
        let edge = (i.min(frames - 1 - i) as f32 / fade as f32).min(1.0);
        let v = (libm::sinf(2.0 * core::f32::consts::PI * hz as f32 * t) * 12_000.0 * edge) as i16;
        samples.push(v);
        samples.push(v);
    }
    match Stream::open().and_then(|mut s| s.write(&samples)) {
        Ok(()) => 0,
        Err(e) => {
            corekit::eprintln!("play: {e}");
            1
        }
    }
}
