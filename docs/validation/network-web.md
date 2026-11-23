# Network and browser reliability follow-up

This work continues the [native UI refresh](ui-refresh.md) on
`codex/calm-native-experience`. Tests use disposable QEMU disks under
`target/network-web`; the user's original `target/waveos-aurora.img` is preserved.
Voyage remains a GTK browser around the packaged WebKit engine. This is not an
engine rewrite or a claim that every website is compatible.

## Changes

- Native DNS validates transaction, record type, response bounds and resolver
  source; asks A/AAAA together with bounded retries and a fixed grace period;
  falls back to framed TCP on truncation. Its cache respects short/zero TTLs,
  caps long TTLs and invalidates on interface/address/router/resolver changes.
  Linux resolver configuration now uses actual active IPv4/IPv6 resolvers,
  without a hard-coded QEMU DNS address.
- Native connection attempts have a total deadline, try alternate IPv6 answers
  and invalidate idle connections/family-failure state on network changes.
  A refused endpoint no longer disables the entire IPv6 family.
- Ethernet link transitions reset dynamic addresses, router/DNS state and
  neighbor caches, renew DHCP/SLAAC, and fail TCP connections from the old
  network promptly. Static IPv4 configuration is retained while routes avoid
  disconnected interfaces. Wi-Fi's existing link notifications share this path.
- Linux poll/epoll expose pending socket errors; `SO_ERROR` consumes its error.
  The new regression test checks a refused loopback connection, unsolicited
  POLLERR/EPOLLERR and a second SO_ERROR read returning zero.
  Connected UDP sockets select a route and report the actual source address,
  including IPv4-mapped addresses on IPv6 sockets; AF_UNSPEC disconnect is
  supported. This addresses the glibc source-address assertion encountered
  during the first Firefox comparison. The [glibc address-sorting code](https://github.com/bminor/glibc/blob/release/2.36/master/sysdeps/posix/getaddrinfo.c)
  uses UDP connect/getsockname to select and compare source addresses.
- UTC is shared between native clients, Linux syscalls and the vDSO. Clock
  corrections republish the vDSO; a UTC-offset edit preserves the instant. The
  QEMU launcher seeds its RTC using the guest's chosen offset instead of the
  host's local time. Additive syscall 83 supplies UTC milliseconds; existing
  identifiers are unchanged. This fixes fresh certificates being rejected as
  not yet valid when host and guest offsets differ.
- Settings separates **Network configured** from verified Internet access.
  **Check connection** runs interface/DNS/TCP/TLS diagnostics outside the UI
  thread, on demand. `netcheck [URL]` provides the same layered native check.
- Voyage provides escaped navigation/certificate explanations, Retry/Back,
  Stop/Escape, download progress/cancellation/completion/failure and unique
  destination names with overwrite disabled. Local absolute paths become file
  URIs. Tab-close requests go through WebKit's before-unload handling.
  TLS verification stays enabled, including the explicit WebKit FAIL policy.
- Wayland surface/buffer damage bounds copying and presentation; geometry,
  viewport and size changes fall back conservatively to full damage. Frame-only
  commits do not repaint. Clients declaring full-window damage are checked
  against cached pixels: identical rows are skipped and visible changes define
  the actual bounds. Unused XRGB alpha does not create false changes; ARGB alpha
  does. Identical scale/viewport/position requests do not invalidate geometry.
  The bridge waits on events when no callback is pending.
  Text keystrokes receive balanced releases despite the native ABI's translated
  character releases lacking identity. Special keys retain normal press/release.

## Reproduction and evidence

`cargo xtask web-check --output target/network-web/check --local-only` builds
a full isolated guest with WebKit, Firefox, schemas and roots. The local fixture
creates a short-lived CA and valid/wrong-identity certificates. That CA belongs
only to the disposable test configuration. The fixture validates the exact
downloaded/uploaded payload, not merely an HTTP success response.

Additional modes include `--net virtio|e1000|e1000e|none`, `--connection-only`,
`--public-only --firefox`, and `--benchmark-only --repetitions 5`.
`--baseline-image PATH` clones an older image and preserves its kernel/bridge,
while updating the browser instrumentation and test roots. Never use a personal
disk as a test output. The scratch account is **Web Review**, password
**wave-test**; baseline clones retain their original scratch account.

The launcher explicitly disables QEMU's implicit default network adapter with
`-nic none` for `--net none`; merely omitting an adapter is not an offline test.

## Validation results

| Check | Result |
| --- | --- |
| Production Rust build; Voyage C build with `-Wall -Werror` | Passed. |
| Host library/xtask tests | 84 passed; includes calendar conversion, DNS parsing, HTTP robustness and TLS interoperability. |
| Wayland renderer host tests | 6 passed; clipped copying/composition, alpha, scaling, identical frames and full-damage refinement. |
| `cargo xtask lint`, root/userland formatting | Passed. |
| Kernel suite, virtio/e1000/e1000e | 40 tests passed on both boots for each adapter, including UTC clocks, UDP source selection and reboot persistence. |
| Voyage local interaction suite | All 21 scenarios passed. |
| Live link loss/recovery | Passed with virtio, e1000 and e1000e. |
| `--net none` | Native diagnostics reported no connection; Voyage displayed an offline error. Passed. |
| Native public DNS and verified HTTPS | `netcheck https://example.com/` passed. |
| Firefox comparison | Loaded/rendered the local fixture after the UDP fix. This is a rendering sanity check, not equivalent full-suite or performance coverage. |

The local scenarios cover real JavaScript activation, UTF-8 content, redirect,
gzip/chunking, valid TLS and rejected wrong-identity TLS, recovery, keyboard and
button Retry, local storage, synthetic sign-in/cookies, downloads with exact byte
counts/distinct names, exact upload payload, transfer cancellation, pending-load
reload/Stop, interrupted response, link recovery, and cookies/storage after reboot.
An additional [fresh-state transfer/persistence run](evidence/network-web/fresh-state-transfer.json)
uses per-run tokens and uploads the just-downloaded unique file, preventing
older fixture data from satisfying these checks.

Raw [local results](evidence/network-web/local-results.json),
[public results](evidence/network-web/public-results.json),
[host tests](evidence/network-web/host-tests.txt),
[renderer tests](evidence/network-web/bridge-tests.txt),
[lint](evidence/network-web/lint.txt), and kernel logs for
[virtio](evidence/network-web/kernel-virtio.txt),
[e1000](evidence/network-web/kernel-e1000.txt), and
[e1000e](evidence/network-web/kernel-e1000e.txt) are included.

![Verified rejection of a wrong-identity certificate](evidence/network-web/certificate-error.png)

![On-demand Settings diagnostics](evidence/network-web/settings-diagnostics.png)

![Download completion with distinct filenames](evidence/network-web/downloads.png)

![Firefox rendering the same local fixture](evidence/network-web/firefox-comparison.png)

### Public-page sample

All five pages produced a successful WebKit load event and visible rendered
content. The optional DOM snapshot can still be `interactive` on a dynamic page;
that state is recorded rather than treated as a network failure. These single
captures do not qualify account sign-in, uploads, every control, or media on the
public sites. Times are this QEMU run's navigation load-event durations, not
recommendations or a speed guarantee.

| Page | Load event | DOM snapshot | Capture |
| --- | ---: | --- | --- |
| DuckDuckGo search | 6.349 s | complete | [Screenshot](evidence/network-web/public-1.png) |
| Wikipedia: Rust | 14.531 s | complete | [Screenshot](evidence/network-web/public-2.png) |
| GitHub repository | 8.319 s | interactive | [Screenshot](evidence/network-web/public-3.png) |
| MDN Web Docs | 1.622 s | interactive | [Screenshot](evidence/network-web/public-4.png) |
| BBC News | 5.795 s | complete | [Screenshot](evidence/network-web/public-5.png) |

## Browser and desktop performance

The preserved pre-network image supplies the original kernel/bridge. Its Linux
browser receives instrumentation so both runs exercise the same WebKit packages
and fixture. Each set launches five new browser processes, with a same-process
revisit in each repetition; disk caches are retained. Both desktops use dark
appearance, Mist, the same window geometry, 1280×800, 4 vCPUs and 4 GiB. No
compilation runs during accepted measurements.

The driver performs scrolling in both directions, a small DOM/button update,
text entry, pointer traversal and Super-drag. Values are frame-weighted mean
compositor paint durations; full-frame counts sum all five repetitions. They
are not frame rate, end-to-end typing latency, or WebKit JavaScript speed.
New-process startup uses the application's monotonic load-event timestamp,
excluding shell launch and QEMU boot.

| Workload | Baseline mean | Updated mean | Full frames, before → after |
| --- | ---: | ---: | ---: |
| Scroll after a new process | 34.71 ms | 29.47 ms | 58 → 54 |
| Scroll after revisit | 37.96 ms | 28.99 ms | 83 → 53 |
| Small DOM/button update | 24.61 ms | 0.88 ms | 13 → 0 |
| Typing | 43.31 ms | 2.04 ms | 42 → 0 |
| Pointer traversal | 4.26 ms | 0.42 ms | 13 → 0 |
| Window dragging | 18.32 ms | 17.43 ms | 100 → 100 |

New-process load-event means were **6.620 s → 6.515 s**; this does not establish
a meaningful startup improvement. Dragging's total paint work was essentially
unchanged (**2.931 s → 2.981 s** across five runs), despite a lower per-frame
mean. Scrolling totals were **2.187 s → 2.299 s** after a new process and
**3.189 s → 2.290 s** on revisit. The clear gains are in small updates, typing
and pointer feedback.

The five-second idle samples recorded 18–20 frames before and 12–16 afterward.
Updated samples contained **zero full frames**, only 936–1248 presented pixels
in total and 5–9 ms of compositor paint per interval. This does not claim that
all application-driven activity stops, or measure idle power consumption.

Raw [baseline](evidence/network-web/baseline-performance.json) and
[updated](evidence/network-web/candidate-performance.json) measurements are
bundled. The matching rendered state is shown below; screenshots alone cannot
demonstrate the performance difference.

| Baseline | Updated |
| --- | --- |
| ![Original bridge](evidence/network-web/baseline-browser.png) | ![Refined bridge](evidence/network-web/candidate-browser.png) |

## Review image

The full disposable review image is `target/network-web/candidate/session.img`.
It has the native UI refresh, current bridge/browser and Linux packages. To
reopen the captured build without refreshing it:

```sh
cargo xtask run --no-build --no-refresh --no-sound --no-keyboard-capture \
  --disk-image target/network-web/candidate/session.img
```

Scratch account: **Web Review**, password **wave-test**. Its fixture downloads
and browser data are disposable. The original `target/waveos-aurora.img` retains
its 30 October modification timestamp and was not used by these checks.

## Scope limits

QEMU uses x86-64 TCG on an ARM Mac, 4 vCPUs, 4 GiB and a 1280×800 display.
Compositor paint duration is not frame rate, browser main-thread execution time,
or physical input latency. Public-site results describe the captured pages,
not every operation on those sites. Fixture sign-in uses synthetic credentials;
no external account was used. DRM, video calls and a new browser engine are
outside this pass.

Physical Ethernet/Wi-Fi adapters, real roaming between networks, display latency
and power consumption have not been qualified. TCP DNS fallback and IPv6-only
fallback have bounded implementations and host/parser coverage; the local
browser suite is not an end-to-end test of every resolver topology. The existing
WebKit disk-cache hard-link limitation remains. The kernel suite on this host
uses `AURORA_TEST_NO_USBREDIR=1`, explicitly skipping unsupported USB-redirection
injection, while retaining its other tests.
