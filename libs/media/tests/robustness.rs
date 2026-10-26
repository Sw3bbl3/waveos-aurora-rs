//! Corrupt video files are refused, never a panic: Nebula demuxes whatever
//! a page points it at.

use nebula_media::fmp4::Demuxer;
use nebula_media::mp4::Video;

const CLIP: &[u8] = include_bytes!("fixtures/flower.mp4");
const FRAGMENTED: &[u8] = include_bytes!("fixtures/flower-fragmented.mp4");

/// The boxes that describe the media sit at the front; the rest is samples.
fn headers(file: &[u8]) -> core::ops::Range<usize> {
    0..file.len().min(8192)
}

#[test]
fn corrupt_movies_never_panic() {
    let seeds = [CLIP.to_vec()];
    aurora_mutate::check_with(
        "mp4",
        &seeds,
        400,
        |m, seed| m.mutate_within(seed, core::slice::from_ref(&headers(seed))),
        |file| {
            let _ = Video::parse(file);
        },
    );
}

#[test]
fn corrupt_fragments_never_panic() {
    let seeds = [FRAGMENTED.to_vec()];
    aurora_mutate::check_with(
        "fmp4",
        &seeds,
        200,
        |m, seed| m.mutate_within(seed, core::slice::from_ref(&headers(seed))),
        |stream| {
            let mut demuxer = Demuxer::new();
            for chunk in stream.chunks(4096) {
                if demuxer.push(chunk).is_err() {
                    break;
                }
            }
        },
    );
}
