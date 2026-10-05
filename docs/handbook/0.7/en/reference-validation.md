# Validation and performance evidence
Release validation distinguishes automated checks, observed UI interactions, measurements, and untested configurations.

## Read the results
1. Open the release’s validation report from the GitHub release assets.
2. Match the reported commit, image checksum, host, emulator, resolution, and workload to your configuration.
3. Compare equivalent metrics: composition/flush time is not end-to-end input latency, and pixels redrawn are not elapsed time.

Desktop measurements use one warm-up and five measured repetitions. Learn is new in 0.7, so startup/search/scroll results are an initial baseline rather than a claimed improvement over a nonexistent older version. Numerical gains appear only when repeatable measurements support them.

## Reproduce
Use `tools/benchmark_desktop.py LOG OUTPUT.json` against the documented VM configuration. Preserve raw frame samples and calculate median and p95 by workload. A development-baseline comparison must be labeled as such rather than described as a previous public release.

## Troubleshooting
If a report lacks a test for your hardware, treat support as unverified. Do not extrapolate a best-case percentage to the whole operating system.

## Measured desktop results — October 5, 2026
The comparison uses development baseline `1011e1a`, not a previous public release, and desktop code at `368d421`. Later handbook and Learn refinements do not change the measured desktop algorithm. Host: Apple M2 (8 cores), 16 GB RAM, macOS 27.0.1 (26A434), QEMU 11.1.1, TCG emulation, q35, x86-64 `-cpu max`, 4 guest CPUs, 512 MiB RAM, AHCI, 1280×800, headless display, networking disabled. Rust nightly-2026-09-28 uses commit d080e7dff1b0fc54541545252818f8cccf995d05 and LLVM 23.1.1.

Each build ran one warm-up followed by five measured repetitions, with Notes and Welcome at the same positions, default light appearance, and no parallel builds. `AURORA_QMP_KEY_DELAY=0.1` controls injected keys. The script replaces one Notes line, drags out and back, switches away and back, and opens/searches/closes Settings. Screenshots after each stage verified placement. The switching workload includes the new chooser, so its visual work differs by design.

All paired values below are baseline / candidate. Time is guest composition plus framebuffer flush per frame batch, in milliseconds. It excludes client drawing, event delivery, and perceived latency. Timing resolution is 1 ms. p95 uses nearest rank; medians pool all measured frame batches. Painted pixels count submitted damage areas, including cursor batches. Baseline `partial-or-cursor` batches are cursor updates; the candidate names full, partial, and cursor paths separately.

| Workload | Samples | Median ms | p95 ms | Mean painted pixels |
|---|---:|---:|---:|---:|
| typing | 180 / 182 | 47 / 10 | 49 / 12 | 1.024e+06 / 71785.9 |
| dragging | 61 / 61 | 45 / 36 | 62 / 69 | 940126 / 621186 |
| switching | 57 / 50 | 28 / 23 | 61 / 69 | 934240 / 519785 |
| settings | 168 / 170 | 50 / 49 | 76 / 73 | 1.024e+06 / 933650 |

Notes typing uses 78.7% less median composition/flush time (47 to 10 ms) and 93.0% fewer painted pixels per batch (1,024,000 to 71,785.9). Each repetition’s median decreased: baseline 46–47 ms, candidate 9–11 ms. This supports the localized-update claim only.

Dragging and switching p95 regressions are material: 62 to 69 ms and 61 to 69 ms respectively. Their mixed frame populations and the new chooser do not support a broad responsiveness claim. Settings’ 50 to 49 ms median difference is too small to advertise as a repeatable improvement. No memory-use or system-wide speedup is claimed.

## Initial Learn measurements
Learn is new. With networking disabled, default 16 px English text and a 1020×620 content area, one warm-up and five measured repetitions produced:

| Operation | Samples | Median ms | p95 ms |
|---|---:|---:|---:|
| Startup through first client draw | 5 | 81 | 87 |
| Search while entering building | 40 | 3 | 4 |
| Client redraw after PageDown/PageUp | 10 | 15.5 | 22 |

Startup begins inside Learn and excludes process spawning and framebuffer presentation. Search times cover filtering/ranking only. Scroll times cover client drawing after the scroll-state change, excluding compositor work and input delivery. These are absolute initial measurements, not improvements over an older Learn version.

## Raw data and reproduction
The [desktop baseline](https://github.com/Sw3bbl3/waveos-aurora-rs/blob/v0.7.0/docs/releases/0.7.0/evidence/desktop-baseline.json), [desktop candidate](https://github.com/Sw3bbl3/waveos-aurora-rs/blob/v0.7.0/docs/releases/0.7.0/evidence/desktop-candidate.json), and [Learn samples](https://github.com/Sw3bbl3/waveos-aurora-rs/blob/v0.7.0/docs/releases/0.7.0/evidence/learn-baseline.json) retain every measured sample and repetition. These links require networking; release validation assets contain offline copies.

1. Build the identified baseline and candidate separately; preserve each system partition and use separate seeded disks.
2. Run `cargo xtask run --no-build --no-refresh --headless --disk-image PATH --net none`, redirecting output to a log. Do not refresh the baseline with the candidate system.
3. Run `AURORA_QMP_KEY_DELAY=0.1 python3 tools/benchmark_desktop.py LOG OUTPUT.json` using the same configuration on both builds.
4. Use `python3 tools/summarize_performance.py BASELINE.json CANDIDATE.json` for statistics.
5. For Learn, start with no Learn window open and English selected; run `tools/benchmark_learn.py LOG OUTPUT.json` with the same key delay.

Host scheduling, TCG emulation, integer timers, and different frame counts limit interpretation. Physical hardware and VirtualBox were not measured.
