# WaveOS Aurora 0.7.0

October 5, 2026

This update introduces Learn, new ways to arrange and switch windows, and improvements to desktop rendering and keyboard navigation.

## Learn
- Browse the complete WaveOS handbook offline in English or Canadian French.
- Find guides with full-text search, follow article contents, and adjust reading size.
- Switch languages while keeping your place in the same article.

## Desktop
- Switch between open and minimized windows with a visual Alt+Tab chooser.
- Arrange windows with edge previews and Super+arrow shortcuts.
- Restore floating window sizes after snapping and retain layouts when display dimensions change.

## Accessibility
- Navigate Settings search, pages, and controls with the keyboard.
- Use consistent activation and slider keys in Settings and Control Center.
- Apply existing contrast, transparency, and motion preferences to new desktop controls.

## Performance
- Eligible localized updates redraw less of the desktop. In a controlled Notes typing workload, median composition and framebuffer-flush time fell from 47 to 10 ms, with 93.0% fewer painted pixels per frame batch against development baseline `1011e1a`.
- Dragging and switching showed higher p95 timings. Read the [measurements and limitations](../../handbook/0.7/en/reference-validation.md#measured-desktop-results-october-5-2026) before applying these results to other workloads.

## Fixes
- Resolves a TCP retransmission issue that could stall transfers on a single-CPU system.

## Known limitations
Native compiler/debugger tools remain unavailable. Browser and hardware limits continue to apply. Read the [complete handbook](../../handbook/0.7/en/start-welcome.md) and [validation guide](../../handbook/0.7/en/reference-validation.md).
