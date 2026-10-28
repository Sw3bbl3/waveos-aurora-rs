# Third-party components

WaveOS uses these vendored components:

| Component | Version | License | Local changes |
|---|---|---|---|
| rusty_h264-decoder | 0.16.0 | [BSD-2-Clause](vendor/rusty_h264-decoder/LICENSE) | [no_std imports and formatting](vendor/rusty_h264-decoder/WAVEOS-PATCHES.md) |
| rusty_aac | 1.1.0 | [Apache-2.0](vendor/rusty_aac/LICENSE) | [decoder-only no_std port and formatting](vendor/rusty_aac/WAVEOS-PATCHES.md) |
| Linux DRM core, virtio-gpu driver, HDMI helpers and DRM/virtio protocol headers | 6.12 | MIT, HPND or BSD-3-Clause, per file (each keeps its notice); see [kernel/linux](kernel/linux/README.md) | Compiled against WaveOS's own MIT LinuxKPI; GPL-only Linux parts are replaced, not used |
| acpi (AML interpreter) | 6.1.1 | [MIT](vendor/acpi/LICENCE-MIT) or [Apache-2.0](vendor/acpi/LICENCE-APACHE) | [stores through references, PCI configuration offsets](vendor/acpi/WAVEOS-PATCHES.md) |
| Wayland protocol descriptions (`wayland.xml`, `xdg-shell.xml`, `xdg-decoration-unstable-v1.xml`, `viewporter.xml`) | wayland 1.21, wayland-protocols 1.31 | MIT, notices inside each file in [linux-programs/wl-bridge/protocols](linux-programs/wl-bridge/protocols) | None; `tools/wayland_protocol.py` generates `wl-bridge`'s `protocol.rs` from them |

Programs downloaded at build time (pinned by SHA-256) and shipped as separate programs in the system image:

| Program | Version | License | Where |
|---|---|---|---|
| BusyBox (Debian's `busybox-static`) | 1.35.0 | GPL-2.0; Debian's copyright file is in `/System/Licenses/busybox.txt`, the source is in Debian's archive | `/System/Linux/bin/busybox`, run as a Linux program |

Built from source in this repository and shipped as a separate program:

| Program | Version | License | Where |
|---|---|---|---|
| doomgeneric (Doom engine, from Chocolate Doom and id Software's Doom source) | commit `e257a09` | [GPL-2.0](third_party/doomgeneric/COPYING), also in `/System/Licenses/doomgeneric.txt`; the source, with the WaveOS backend, is in [third_party/doomgeneric](third_party/doomgeneric/README.md) | `/System/Games/Doom/doom`, run as a Linux program |

The Doom app downloads Freedoom 0.13.0 (BSD-3-Clause) only when you ask it to; its license is saved next to the game data as `Freedoom License.txt`.

The interface typefaces ship in `/System/Fonts` next to their licenses:

| Font | License | Local changes |
|---|---|---|
| Manrope (Light, Medium, Bold, ExtraBold) | [SIL OFL 1.1](assets/fonts/LICENSE-Manrope.txt) | Static instances cut from the variable font, overlaps removed |
| JetBrains Mono | [SIL OFL 1.1](assets/fonts/LICENSE-JetBrainsMono.txt) | None |

The bootable system includes the decoder license texts under `/System/Licenses`. Cargo manifests and lockfiles identify registry dependencies, including `rusty_h264-common` 0.16.0 and the `regress` regular-expression engine. Their upstream copyright and licensing remain applicable.

Media test fixtures have separate provenance and terms, documented in [the fixture README](libs/media/tests/fixtures/README.md). The MP4 sample is CC0; the AAC tone is generated test data. Downloaded third-party website scripts used during development are not redistributed in release assets.
