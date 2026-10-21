//! Host validation of the same no_std demuxer/decoder used by WaveOS.
use std::{fs, io::Write, time::Instant};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let data = fs::read(&args[1]).unwrap();
    let video = nebula_media::mp4::Video::parse(&data).unwrap();
    println!(
        "{}x{}, {}ms, {} samples, audio={}",
        video.width,
        video.height,
        video.duration_ms,
        video.samples.len(),
        video.has_audio
    );
    let mut decoder = nebula_media::VideoDecoder::new(&video.config).unwrap();
    let start = Instant::now();
    let mut frames = 0;
    let mut last = None;
    let mut changed = 0;
    for (i, sample) in video.samples.iter().enumerate() {
        if let Some(frame) = decoder.decode(&data[sample.offset..sample.offset + sample.size]).unwrap() {
            let pixels = nebula_media::pixels(&frame).unwrap();
            let hash = pixels.iter().fold(0u64, |a, &p| a.wrapping_mul(31).wrapping_add(p as u64));
            if last.is_some_and(|v| v != hash) {
                changed += 1;
            }
            last = Some(hash);
            if let Some(dir) = args.get(2) {
                if i == 0 || i == video.samples.len() / 2 {
                    let mut out = fs::File::create(format!("{dir}/frame-{i}.ppm")).unwrap();
                    write!(out, "P6\n{} {}\n255\n", frame.width, frame.height).unwrap();
                    for p in pixels {
                        out.write_all(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]).unwrap();
                    }
                }
            }
            frames += 1;
        }
    }
    println!("decoded {frames} frames, {changed} changes in {:?}", start.elapsed());
    assert!(frames > 1 && changed > 0, "expected moving video");
}
