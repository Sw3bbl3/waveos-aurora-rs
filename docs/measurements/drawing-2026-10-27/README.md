# Drawing speed, 27 October 2026

What it measures: the drawing primitives behind one desktop frame at
1280 × 800 — the wallpaper, three windows with shadows, rounded corners and
borders, six cached glass panels, 40 lines of text, 10 icons and a
full-screen translucent fill — plus the login screen's background blur.
`libs/gfx/examples/frame_bench.rs` prints the median of many runs of each.

- `before-7ae5990.txt`: commit 7ae5990 (before this round), with the same
  benchmark copied in. That commit has no `blur_coarse`; its login screen used
  the full blur, so both blur numbers are the full blur.
- `after.txt`: commit dfcf009.

Both ran on the host (4-core Intel Xeon, rustc 1.101.0-nightly 2026-09-27,
release build), one after the other. On the host the SSE2 paths are on, as
they are in the compositor's kernel task.

| | Before | After |
|---|---|---|
| Frame (sum of primitives) | 6.00 ms | 2.72 ms |
| Window shadows | 2.26 ms | 0.49 ms |
| Full-screen translucent fill | 1.78 ms | 0.58 ms |
| Glass panels | 0.67 ms | 0.45 ms |
| Window borders | 0.19 ms | 0.07 ms |
| Login background blur | 28.9 ms | 4.9 ms |

Three repeated runs of each gave frames of 5.97–6.12 ms before and
2.59–2.77 ms after, and login blurs of 30.2–30.9 ms before and
4.87–5.01 ms after.

These are host CPU timings of the drawing code, not frame rates inside
WaveOS: the development virtual machines run under software emulation
(QEMU TCG), where everything is many times slower and timings swing with
the host's load.

Reproduce: `cargo run --release -p lumen --example frame_bench`.
