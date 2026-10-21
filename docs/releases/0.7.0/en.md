# WaveOS Aurora 0.7.0

October 21, 2026

This release brings a searchable offline handbook, new desktop and file workflows, and the Pulsar JavaScript engine with native browser media support.

## Learn
- Browse 60 offline articles across English and Canadian French, including user guides, developer instructions, and release notes.
- Search full article text, follow contents, adjust reading size, and switch languages while retaining the article.

## Desktop and Files
- Choose open and minimized windows with a visual Alt+Tab switcher.
- Arrange windows with edge previews and Super+arrow shortcuts, then restore their floating size.
- Work with up to eight Files tabs and background transfers with progress, conflict choices, and cancellation.

## Nebula and Pulsar
- Run supported JavaScript with DOM events, timers, fetch, storage, cookies, and an interactive Terminal prompt.
- Use browser tabs, shared storage events, 2D canvas, and additional DOM/CSS interfaces.
- Reuse compiled scripts and cached property lookups, and defer eligible function compilation until needed.
- Try experimental native H.264/AAC-LC MP4 playback and fragmented MP4 media-source support.

## Networking and accessibility
- IPv6 adds neighbor discovery, SLAAC, ICMPv6, TCP, and UDP alongside the existing IPv4 stack.
- Search Settings pages and individual controls, and navigate with consistent keyboard activation and slider keys.
- Use existing motion, transparency, and contrast preferences throughout the desktop and Learn.

## Performance
- Localized desktop updates repaint eligible changed regions. In the identified October 5 development comparison, median Notes composition/flush time fell from 47 to 10 ms, with 93.0% fewer painted pixels per frame batch.
- These are historical measurements of the named builds, before the later browser/media integration. Dragging and switching p95 timings regressed in that comparison. Read the [measurement scope and raw evidence](../../handbook/0.7/en/reference-validation.md#measurement-scope); no extra speedup percentage is claimed for the combined release.

## Fixes
- Corrects TCP retransmission acknowledgement handling that could stall single-CPU transfers.
- Keeps JavaScript values rooted across browser callbacks and promise jobs to avoid premature collection.
- Retains a bounded EFI optical-boot image while keeping full host kernel debugging symbols available separately.

## Known limitations
Native Rust compiler/debugger tools remain unavailable. Web compatibility is partial; JavaScript modules, BigInt, Intl, DRM, and general YouTube/TikTok playback are not established. Physical hardware and VirtualBox are not certified by QEMU checks. Read the [complete handbook](../../handbook/0.7/en/start-welcome.md) and [validation guide](../../handbook/0.7/en/reference-validation.md).
