`flower.mp4` is the CC0 sample used by MDN interactive examples:
https://interactive-examples.mdn.mozilla.net/media/cc0-videos/flower.mp4

Retrieved 2026-10-10. H.264 video, 960 by 540, 150 frames, about five
seconds, with an audio track. Used to exercise real MP4 sample tables,
B-frame presentation timestamps, and decoding; not a YouTube fixture.

`tone.aac` is a generated one-second stereo test signal: 440 Hz left,
660 Hz right, 48 kHz, encoded as 128 kbit/s AAC-LC ADTS using macOS
`afconvert`. The source sine amplitudes are 8000 and 6000 in 16-bit PCM.
It is original test data, used to verify frequency and channel separation.

`flower-fragmented.mp4` is `flower.mp4`'s video track remuxed into an
initialization segment and one media segment per keyframe (the layout DASH
and YouTube use), by `write_fragmented_fixture` in `tests/fmp4.rs`.
