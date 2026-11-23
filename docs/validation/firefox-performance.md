# Firefox / YouTube follow-up

The reported problem occurs both while browsing a paused YouTube page and during
playback. This follow-up runs on `codex/calm-native-experience` and preserves the
existing UI and network work. All browser experiments use disposable disks under
`target/firefox-performance`; no validation command targets the personal disk.

## Changes

- `proc::readiness::Watch` registers interest before checking readiness, remembers
  matching notifications through the check/sleep interval, and unregisters on
  drop. Unrelated objects cannot cancel the sleep through a global generation
  counter. Linux poll/select, epoll, eventfd and local socket waits share it.
  Epoll registration holds its interest lock while subscribing, so concurrent
  ADD/MOD operations cannot fall between gathering keys and registration.
- Disabled EPOLLONESHOT entries contribute neither wake keys nor timer deadlines.
  An expired, already-delivered timer therefore cannot repeatedly force a 1 ms
  rescan. MOD still rearms it and immediately reports existing readiness.
- Network timer maintenance no longer broadcasts socket readiness every 10 ms
  without a change. Received packets, link changes and TCP timeout/closure changes
  still notify waiters. The network maintenance tick itself remains active.
- Firefox defaults to decoding in its existing content process. On this Linux
  compatibility layer, the remote decoder configuration buffers media but stalls
  before the first decoded frame. The defaults set `media.rdd-process.enabled`
  and `media.utility-process.enabled` false, and
  `media.allow-audio-non-utility` true. Existing user preferences take precedence.
  This reduces decoder crash isolation. It does not implement the Linux sandboxes
  already missing from this port. No codec, resolution or frame-rate cap is added.
- The Wayland bridge preserves partial damage for explicit identity viewports,
  including integer display scales. Actual scaling/cropping retains full damage.
  Firefox's use of a viewport no longer automatically expands a caret/video
  update to its full window.
- `AURORA_SESSION_DIR` isolates QEMU control sockets and firmware variables.
  `tools/qmp.py` and the web smoke runner use the same directory. This allows
  disposable checks alongside an interactive VM without taking over its socket.

The media fallback follows the existing paths in Mozilla's
[ESR 140 PDM factory](https://github.com/mozilla-firefox/firefox/blob/esr140/dom/media/platforms/PDMFactory.cpp)
and [preference definitions](https://github.com/mozilla-firefox/firefox/blob/esr140/modules/libpref/init/StaticPrefList.yaml).
It is a compatibility workaround, not proof that the remote process startup/IPC
implementation is complete.

## Reproduction

Build with `cargo xtask build`. Clone a stopped disposable disk; never use a
personal disk for this experiment. Start it with a unique absolute session path:

```sh
AURORA_SESSION_DIR="$PWD/target/firefox-performance/session" cargo xtask run \
  --no-build --headless --no-sound --no-keyboard-capture \
  --disk-image target/firefox-performance/final.img
```

In the scratch guest, start Firefox with `--remote-debugging-port 9222` and the
YouTube URL. Add QEMU forwarding for **127.0.0.1 only** using the private QMP socket:
`hostfwd_add net0 tcp:127.0.0.1:9222-:9222`. Run:

```sh
AURORA_SESSION_DIR="$PWD/target/firefox-performance/session" \
  python3 tools/benchmark_firefox.py target/firefox-performance/final-warm-1.json \
  --log target/firefox-performance/final-serial.log --pid QEMU_PID
```

The BiDi probe is an explicitly enabled test facility. Normal Firefox launches do
not enable remote control or port forwarding. BiDi recommended automation
preferences are active in these instrumented runs; this is not a stock-browser
security or preference audit.

The workload uses the Blender Foundation's Big Buck Bunny YouTube watch page,
`https://www.youtube.com/watch?v=aqz-KE-bpKQ`, anonymous and muted. It waits for the
watch-page document and video metadata, settles for ten seconds, measures eight
seconds paused, types a temporary search query through native keyboard input,
scrolls through native wheel input, and attempts twenty seconds of playback.
It records codec support/selection, frame counters, buffering, media time, rAF
gaps, serial compositor timings and host QEMU CPU time. It creates no documents.

rAF gaps measure main-thread scheduling, not input-to-display latency or displayed
FPS. Playback counters belong to Firefox's media pipeline. Compositor timings do
not include all browser decoding, JavaScript, networking or host presentation.
Public-page content and network timing vary between runs. CPU percentages use
100% per host core. A separate interactive VM was running during final checks,
so CPU/wall-time comparisons across those runs are not controlled benchmarks.

## Diagnosis and experiments

The initial CPU samples repeatedly landed in epoll/readiness scans and allocator
or lock paths. Those samples motivated the scoped wait changes. A loaded baseline
watch page still had healthy buffering but `readyState = 1`, media time zero and
zero decoded frames; playback did not begin within the test timeout.

Kernel changes alone did not restore playback. Disabling AV1 in the disposable
profile did not restore playback either. The production configuration therefore
keeps AV1 available. Keeping the RDD process enabled while disabling only the
utility process also stalled at zero frames. With both remote decoder processes
disabled and content decoding allowed, the diagnostic run advanced 20.46 seconds,
with 568 total video frames and 144 dropped frames at 854 × 480. Verbose decoder
logging was active for that diagnostic run; it is not the final performance result.

## Validation

- Production build, formatting and lint: passed before final isolated checks.
- Kernel suite: **41 passed on each of two boots**, including the new scoped-wait
  regression, signal interruption, eventfd/epoll/timerfd, socket errors, TCP/UDP,
  clocks and persistence. Virtual USB-redirection injection was explicitly skipped
  (`AURORA_TEST_NO_USBREDIR=1`); other configured kernel tests ran.
- Renderer host suite: **7 passed**, including a pixel-for-pixel comparison of
  partial versus full identity-viewport repaint at 1× and 2×, with real cropping
  and scaling retaining conservative damage.
- xtask host suite: **13 passed**.

Final browser measurements and evidence are recorded below after completion.

## Boundaries

These are x86-64 QEMU TCG checks on an ARM Mac, four guest CPUs, 4 GiB, initially
1280 × 800 with the standard virtual display. Software decoding and rendering
remain expensive under emulation. This does not qualify 4K/60 playback, account
sign-in, protected media, audible output or physical hardware acceleration.
