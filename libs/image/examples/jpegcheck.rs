//! Compares our JPEG decoder with jpeg-decoder on files:
//! `cargo run -p aurora-image --example jpegcheck -- a.jpg b.jpg …`
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--same") {
        let a: Vec<String> = std::env::args().skip(2).collect();
        return compare_ours(&a[0], &a[1]);
    }
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let ours = aurora_image::decode(&bytes);
        let mut dec = jpeg_decoder::Decoder::new(&bytes[..]);
        let theirs = dec.decode();
        match (ours, theirs) {
            (Ok(img), Ok(px)) => {
                let info = dec.info().unwrap();
                let ch = px.len() / (info.width as usize * info.height as usize);
                let mut worst = 0;
                let mut total = 0u64;
                for (i, p) in img.pixels.iter().enumerate() {
                    let t = &px[i * ch..];
                    let their = if ch == 1 {
                        [t[0]; 3]
                    } else if ch == 4 {
                        // CMYK (as jpeg-decoder returns it): to RGB.
                        let k = t[3] as u32;
                        [(t[0] as u32 * k / 255) as u8, (t[1] as u32 * k / 255) as u8, (t[2] as u32 * k / 255) as u8]
                    } else {
                        [t[0], t[1], t[2]]
                    };
                    let our = [(p >> 16) as u8, (p >> 8) as u8, *p as u8];
                    for k in 0..3 {
                        let d = (our[k] as i32 - their[k] as i32).abs();
                        worst = worst.max(d);
                        total += d as u64;
                    }
                }
                let mean = total as f64 / (img.pixels.len() * 3) as f64;
                println!("{path}: {}x{} ({ch}ch) worst {worst} mean {mean:.2}", img.width, img.height);
            }
            (a, b) => println!("{path}: ours {:?}, theirs {:?}", a.map(|i| (i.width, i.height)), b.map(|v| v.len())),
        }
    }
}

#[allow(dead_code)]
fn compare_ours(a: &str, b: &str) {
    let x = aurora_image::decode(&std::fs::read(a).unwrap()).unwrap();
    let y = aurora_image::decode(&std::fs::read(b).unwrap()).unwrap();
    let mut total = 0u64;
    for (p, q) in x.pixels.iter().zip(&y.pixels) {
        for s in [0, 8, 16] {
            total += (((p >> s) & 255) as i64 - ((q >> s) & 255) as i64).unsigned_abs();
        }
    }
    println!("{a} vs {b}: mean {:.2}", total as f64 / (x.pixels.len() * 3) as f64);
}
