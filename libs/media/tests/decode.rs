const CLIP: &[u8] = include_bytes!("fixtures/flower.mp4");

#[test]
fn decodes_aac_track_to_real_stereo_pcm() {
    let video = nebula_media::mp4::Video::parse(CLIP).unwrap();
    let audio = video.audio.expect("AAC track");
    let mut decoder = nebula_media::AudioDecoder::new(&audio.config).unwrap();
    let mut count = 0;
    let mut energy = 0u64;
    for s in audio.samples {
        let pcm = decoder.decode(&CLIP[s.offset..s.offset + s.size]).unwrap();
        assert_eq!(pcm.len() % 2, 0);
        count += pcm.len();
        energy += pcm.iter().map(|&v| (v as i64 * v as i64) as u64).sum::<u64>();
    }
    assert!((470_000..510_000).contains(&count), "{count} samples");
    // This fixture has a genuinely silent AAC track (also checked with the
    // macOS decoder). The separate generated tone verifies nonzero sound.
    assert_eq!(energy, 0);
}

#[test]
fn aac_tone_preserves_pitch_and_channel_separation() {
    let mut bytes = &include_bytes!("fixtures/tone.aac")[..];
    let mut decoder = nebula_media::AudioDecoder::new(&[0x11, 0x90]).unwrap();
    let mut pcm = Vec::new();
    while !bytes.is_empty() {
        assert_eq!(bytes[0], 0xff);
        assert_eq!(bytes[1] & 0xf7, 0xf1); // MPEG-2 or MPEG-4, no CRC
        let size = (((bytes[3] & 3) as usize) << 11) | ((bytes[4] as usize) << 3) | (bytes[5] as usize >> 5);
        pcm.extend(decoder.decode(&bytes[7..size]).unwrap());
        bytes = &bytes[size..];
    }
    for (channel, hz) in [(0, 440), (1, 660)] {
        let samples: Vec<_> = pcm.chunks_exact(2).skip(8000).take(24000).map(|s| s[channel]).collect();
        let crossings = samples.windows(2).filter(|s| s[0] < 0 && s[1] >= 0).count();
        assert!((hz / 2 - 2..=hz / 2 + 2).contains(&crossings), "channel {channel}: {crossings}");
        assert!(samples.iter().any(|s| s.unsigned_abs() > 5000));
    }
}

#[test]
fn decodes_real_mp4_frames_and_composition_timestamps() {
    let video = nebula_media::mp4::Video::parse(CLIP).unwrap();
    assert_eq!((video.width, video.height, video.samples.len()), (960, 540, 150));
    assert!(video.has_audio);
    assert_eq!(video.duration_ms, 5005);
    let mut decoder = nebula_media::VideoDecoder::new(&video.config).unwrap();
    let mut hashes = std::collections::BTreeSet::new();
    for sample in &video.samples {
        let frame = decoder.decode(&CLIP[sample.offset..sample.offset + sample.size]).unwrap().unwrap();
        assert_eq!((frame.width, frame.height), (960, 540));
        let pixels = nebula_media::pixels(&frame).unwrap();
        hashes.insert(pixels.iter().fold(0u64, |n, &p| n.wrapping_mul(31).wrapping_add(p as u64)));
    }
    assert_eq!(hashes.len(), 150);
    let mut times: Vec<_> = video.samples.iter().map(|s| s.time_ms).collect();
    times.sort();
    assert!(times.windows(2).all(|w| (33..=34).contains(&(w[1] - w[0]))));
}

#[test]
fn malformed_and_truncated_mp4_are_rejected_without_panics() {
    for end in (0..CLIP.len()).step_by(997) {
        assert!(nebula_media::mp4::Video::parse(&CLIP[..end]).is_err());
    }
    for bytes in [&b"\0\0\0\x01moov"[..], &b"\0\0\0\x07moov"[..], &b"\xff\xff\xff\xffmoov"[..]] {
        assert!(nebula_media::mp4::Video::parse(bytes).is_err());
    }
    let video = nebula_media::mp4::Video::parse(CLIP).unwrap();
    let mut decoder = nebula_media::VideoDecoder::new(&video.config).unwrap();
    assert!(decoder.decode(&[0, 0, 0, 255, 1]).is_err());
    assert!(decoder.decode(&[0, 0, 0, 0]).is_err());
    for end in 0..7 {
        assert!(nebula_media::VideoDecoder::new(&video.config[..end]).is_err());
    }
}
