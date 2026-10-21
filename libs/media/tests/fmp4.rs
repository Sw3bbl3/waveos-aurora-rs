//! Fragmented MP4 demuxing, checked against a stream remuxed here from the
//! progressive fixture (same samples, same timing).
use nebula_media::fmp4::{Codec, Demuxer, Event, Frame};
use nebula_media::mp4::{Sample, Video};

const CLIP: &[u8] = include_bytes!("fixtures/flower.mp4");

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = (((version as u32) << 24) | flags).to_be_bytes().to_vec();
    b.extend_from_slice(body);
    bx(kind, &b)
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// A minimal initialization segment: one AVC track, timescale 1000.
fn init(video: &Video) -> Vec<u8> {
    let mut mvhd = vec![0; 8];
    mvhd.extend(be32(1000));
    mvhd.extend(be32(video.duration_ms as u32));
    mvhd.extend([0; 80]);
    let mut tkhd = vec![0; 8];
    tkhd.extend(be32(1));
    tkhd.extend([0; 68]);
    let mut mdhd = vec![0; 8];
    mdhd.extend(be32(1000));
    mdhd.extend([0; 8]);
    let mut avc1 = vec![0; 24];
    avc1.extend(video.width.to_be_bytes());
    avc1.extend(video.height.to_be_bytes());
    avc1.extend([0; 50]);
    avc1.extend(bx(b"avcC", &video.config));
    let mut stsd = be32(1).to_vec();
    stsd.extend(bx(b"avc1", &avc1));
    let stbl = bx(b"stbl", &full(b"stsd", 0, 0, &stsd));
    let minf = bx(b"minf", &stbl);
    let mdia = bx(b"mdia", &[full(b"mdhd", 0, 0, &mdhd), minf].concat());
    let trak = bx(b"trak", &[full(b"tkhd", 0, 3, &tkhd), mdia].concat());
    let mut trex = be32(1).to_vec();
    trex.extend(be32(1));
    trex.extend([0; 12]);
    let mvex = bx(b"mvex", &full(b"trex", 0, 0, &trex));
    [bx(b"ftyp", b"iso5\0\0\0\0iso5dash"), bx(b"moov", &[full(b"mvhd", 0, 0, &mvhd), trak, mvex].concat())].concat()
}

/// One media segment (moof + mdat) for `samples`, starting at decode time `dts`.
fn segment(samples: &[Sample], dts: u64, seq: u32) -> Vec<u8> {
    let mut trun = be32(samples.len() as u32).to_vec();
    trun.extend(be32(0)); // data offset, patched below
    for (i, s) in samples.iter().enumerate() {
        let decode = dts + samples[..i].iter().map(|s| s.duration_ms).sum::<u64>();
        trun.extend(be32(s.duration_ms as u32));
        trun.extend(be32(s.size as u32));
        trun.extend(be32(if s.sync { 0x0200_0000 } else { 0x0101_0000 }));
        trun.extend(be32((s.time_ms as i64 - decode as i64) as i32 as u32));
    }
    let traf = |trun: &[u8]| {
        bx(
            b"traf",
            &[
                full(b"tfhd", 0, 0x02_0000, &be32(1)),
                full(b"tfdt", 1, 0, &dts.to_be_bytes()),
                full(b"trun", 1, 0x0f01, trun),
            ]
            .concat(),
        )
    };
    let moof_len = bx(b"moof", &[full(b"mfhd", 0, 0, &be32(seq)), traf(&trun)].concat()).len();
    trun[4..8].copy_from_slice(&be32(moof_len as u32 + 8));
    let moof = bx(b"moof", &[full(b"mfhd", 0, 0, &be32(seq)), traf(&trun)].concat());
    let data: Vec<u8> = samples.iter().flat_map(|s| CLIP[s.offset..s.offset + s.size].to_vec()).collect();
    [moof, bx(b"mdat", &data)].concat()
}

fn remux() -> (Video, Vec<u8>) {
    let video = Video::parse(CLIP).unwrap();
    let mut stream = init(&video);
    // Fragments at each keyframe, as DASH segments are cut.
    let starts: Vec<usize> = (0..video.samples.len()).filter(|&i| video.samples[i].sync).collect();
    let mut dts = 0;
    for (k, &start) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(video.samples.len());
        stream.extend(segment(&video.samples[start..end], dts, k as u32 + 1));
        dts += video.samples[start..end].iter().map(|s| s.duration_ms).sum::<u64>();
    }
    (video, stream)
}

fn demux(stream: &[u8], split: usize) -> (Vec<Codec>, Vec<Frame>) {
    let mut d = Demuxer::new();
    let (mut codecs, mut frames) = (Vec::new(), Vec::new());
    for chunk in stream.chunks(split) {
        for e in d.push(chunk).unwrap() {
            match e {
                Event::Init(i) => codecs.extend(i.tracks.into_iter().map(|t| t.codec)),
                Event::Frames(f) => frames.extend(f),
            }
        }
    }
    assert_eq!(d.pending(), 0);
    (codecs, frames)
}

#[test]
fn fragments_round_trip_samples_and_timing_in_any_split() {
    let (video, stream) = remux();
    for split in [stream.len(), 4096, 1000, 7] {
        let (codecs, frames) = demux(&stream, split);
        assert_eq!(codecs, vec![Codec::Avc { config: video.config.clone(), width: video.width, height: video.height }]);
        assert_eq!(frames.len(), video.samples.len());
        for (f, s) in frames.iter().zip(&video.samples) {
            assert_eq!(&f.data[..], &CLIP[s.offset..s.offset + s.size]);
            assert_eq!(f.pts, s.time_ms as i64 * 1000);
            assert_eq!(f.duration, s.duration_ms as i64 * 1000);
            assert_eq!(f.sync, s.sync);
        }
        assert!(frames.windows(2).all(|w| w[1].dts > w[0].dts));
    }
}

#[test]
fn demuxed_fragments_decode() {
    let (video, stream) = remux();
    let (_, frames) = demux(&stream, 65536);
    let mut decoder = nebula_media::VideoDecoder::new(&video.config).unwrap();
    let mut pictures = 0;
    for f in frames.iter().take(30) {
        if decoder.decode(&f.data).unwrap().is_some() {
            pictures += 1;
        }
    }
    assert!(pictures >= 25, "{pictures} pictures");
}

#[test]
fn media_before_init_and_bad_sizes_are_errors() {
    let (_, stream) = remux();
    let init_len = {
        let ftyp = u32::from_be_bytes(stream[0..4].try_into().unwrap()) as usize;
        ftyp + u32::from_be_bytes(stream[ftyp..ftyp + 4].try_into().unwrap()) as usize
    };
    assert!(Demuxer::new().push(&stream[init_len..]).is_err());
    let mut bad = stream[..init_len].to_vec();
    bad.extend([0, 0, 0, 3, b'm', b'o', b'o', b'f']);
    assert!(Demuxer::new().push(&bad).is_err());
}

/// Regenerates `fixtures/flower-fragmented.mp4` (used by the browser's MSE
/// tests): `cargo test -p nebula-media --test fmp4 -- --ignored`.
#[test]
#[ignore]
fn write_fragmented_fixture() {
    let (_, stream) = remux();
    std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/flower-fragmented.mp4"), stream).unwrap();
}
