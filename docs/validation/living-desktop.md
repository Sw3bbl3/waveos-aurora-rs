# Living desktop implementation and validation

Branch `codex/living-desktop` was rebased onto main `b68cf50`, including the upstream NetSurf merge.
No release or merge is authorized by this change. Claude's original checkout and its disk images are untouched.

## Implemented

- Destination-volume staged saves, distinct publication/durability results, Notes rename retry and Trash dirty-state fixes.
- Staged cross-volume Trash moves/restores with original-path metadata persisted before removal and streaming copies.
- Paint workspace, adaptive inspector (a sheet in narrow windows), hexadecimal colour entry, tool-specific controls, panning, anchored zoom, and Save/Discard/Cancel and replacement prompts.
- `/Desktop` background snapshots and operations, automatic right-to-left column layout, sorting, scrolling, selection, keyboard operations, context menus, Files transfer launches, file/app shortcuts, and shared cut clipboard metadata.
- Bounded interacting water heights, wallpaper refraction/lighting, active-only updates and damage, reduced render detail under pressure, Appearance switch and Reduce Motion support.
- Required build support: Claude's macOS Linux compiler fix, extracted Debian cache completeness validation, and two ARM host lint fixes.

## Verified so far (2026-10-29)

- `cargo xtask build`: kernel, userland and Linux programs built and packaged; QEMU booted at 1280×800 on a disposable image.
- Shared model tests: safe-save fault injection, read-only/full destinations, rename failure/retry, final flush identity, temp-name collisions, shortcut validation, layout/sorting/overflow.
- Production kernel Trash host adapter: successful cross-volume round trip, each publication/flush failure, source deletion failure, full disk, hidden file restoration, replacement conflicts, emptying.
- Production kernel clipboard host adapter: compatible readers, cut metadata, failed/cancelled operation retention, confirmed-move removal, unrelated copied/text clipboard preservation.
- `cargo xtask lint`, both workspace formatting checks, `cargo xtask docs` and the rebased production build passed.
- Host tests for AuroraFS, FAT32, Lumen, images and xtask passed. The production Files transfer worker passed 12 fault and conflict tests.
- Kernel suite: 40 tests passed on each of two disposable boots, including persistence and S3 wake. The virtual USB HID/controller test was explicitly skipped because the Mac QEMU binary lacks `usb-redir`; its normal USB hub, keyboard, tablet and storage tests passed.
- QEMU: Paint brush/line/rectangle/ellipse, undo/redo, Fit/100%, space-drag pan, PNG save, close Cancel, desktop folder creation/rename, launcher Add to Desktop and opening its Paint shortcut; narrow inspector inspected.
- QEMU Files `--self-check`: cross-volume transfer, tab isolation and cancel-on-close PASS; final `FILES CHECK PASS` in serial log.
- User-created desktop folder and shortcut survived restart. Cut/paste into a folder, dragging back to the desktop, dragging to Trash, Put Back, external file refresh, and desktop focus isolation were exercised on a disposable image. A dangerous Trash routing bug discovered during testing was fixed and retested.
- Paint overwrite confirmation and saving before opening a dropped image were exercised in QEMU.
- Light and dark Paint/desktop screenshots captured at 1280×800, 1920×1080 and 2560×1600 with 2× scaling; the light HiDPI Paint shot was recaptured after its first frame. An APNG of overlapping ripples was captured.
- Existing host frame benchmark: five interleaved matched runs; baseline median 2.543 ms, updated 2.553 ms (+0.393%). This benchmark models rendering primitives, not the running OS or physical hardware.
- Water host benchmark on the final code: 1280×800 median 3.235 ms / p95 5.613 ms; 1920×1080 5.277 / 9.966 ms; 2560×1600 scale 2: 11.591 / 22.329 ms. Simulation ~0.050 ms. No per-click/frame allocations; the simulation stops advancing after settling. These are host measurements, not physical-hardware measurements.

- On the rebased build, the GINA check verified that a package shortcut resolves to its updated active version. A Paint save completed with destination-volume synchronization.
- QEMU screenshots showed no wallpaper changes when water effects were disabled or Reduce Motion was enabled. With both settings allowing the effect, a click changed the wallpaper during the wave and the image returned pixel-for-pixel to its prior state after settling. Frame logs returned to idle after the last settling and clock updates.

## Limits

QEMU transfer conflict/cancellation UI and exhaustive Paint tool/exit combinations have not been fully exercised; the production transfer worker has host fault coverage for conflicts and cancellation. HiDPI waves under QEMU software emulation produced some slow frames, and physical-hardware responsiveness has not been measured. Virtual USB HID/controller checks require a QEMU build with usb-redir and remain untested locally.

Physical hardware has not been tested. Raw logs, screenshots, disposable images and recordings are under the isolated checkout's `target/validation` or task-local `/tmp/waveos-*`; selected evidence will be linked below at delivery.
