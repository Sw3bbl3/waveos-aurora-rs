# Roadmap

WaveOS Aurora grows in milestones. Each one ends with something you can boot and use.

## ✅ M1: Boot to a graphical desktop

- The UEFI bootloader, and a higher-half kernel with physical memory management and a heap.
- ACPI, APIC timer and IRQ routing, a preemptive scheduler, PS/2 input, the RTC, and ACPI power off and restart.
- The Lumen Server compositor and the Aurora desktop: menu bar, dock, launcher, windows, light and dark themes, and wallpapers.
- The apps: Files, Terminal, Notes, Calculator, Settings, About and Welcome, all on an in-memory RamFS.
- A kernel self-test suite, headless CI boots, and a USB-bootable GPT image.

## ✅ M2: User space

- **Ring 3:** per-process address spaces with NX, `syscall`/`sysret`, and a validated user-pointer ABI (`libs/abi`).
- **Processes:** an ELF loader and spawn/wait/kill. Kills are cooperative, and a reaper task frees exited processes.
- **IPC and isolation:** fd tables and pipes. A CPU fault in ring 3 kills only the faulting process, and the desktop shows a crash report.
- **Lumen Server:** stays in the kernel, like NT's win32k. Apps draw into shared-memory surfaces and receive events through `next_event`.
- **Userland:** the corekit runtime and the AuroraKit UI toolkit. All seven apps now run in user space.
- **Terminal:** a real shell. Programs in `/System/Bin` run as separate processes, with pipelines and redirection.
- **Tests:** the `usertest` syscall conformance suite runs as part of `cargo xtask test`.
- **Deferred:** message-port IPC and a user-space window server are planned together with multi-threaded processes.

## ✅ M3: Storage and AuroraFS

- **Hardware:** PCI/PCIe enumeration (ECAM via ACPI MCFG). Polled drivers for **AHCI**, **virtio-blk** (modern virtio-pci) and **NVMe**, plus GPT partitions.
- **VFS:** a mount table and a write-back block cache, with a background flusher every 5 s and a sync on shutdown and restart.
- **AuroraFS:** our journaled, extent-based filesystem.
  - Metadata goes through a physical write-ahead journal in ordered mode, with revoke-on-free and checksummed commits.
  - It's a `no_std` library shared by the kernel and the host `mkfs`, with crash-recovery tests.
- **FAT32:** read/write with long file names, mounted at `/Boot`, and cross-checked against the `fatfs` crate.
- **Disk images:** `xtask` builds GPT images with an ESP plus a AuroraFS home volume, and keeps the home volume across rebuilds.
- **Files:** volumes in the sidebar, New Folder, inline Rename, Delete, Copy/Cut/Paste and context menus. The theme and wallpaper persist.
- **Tests:** the kernel suite runs on all three controllers in CI, and boots twice to verify persistence.
- **Deferred:** interrupt-driven I/O (MSI/MSI-X) moves to M5, and drag and drop moves to M4.

## ✅ M4: Apps and polish

- **Text:** our own TrueType engine (outlines, kerning, exact-area anti-aliasing in fixed point) renders any size, in the kernel and in apps.
- **User space:** SSE2 for programs, threads, futexes, `Mutex`/`Condvar`.
- **New apps:** Preview (our own PNG and BMP codecs), Paint, Clock (world clock, alarms, stopwatch, timers) and Activity Monitor.
- **Notes:** selection, the clipboard, undo/redo, find, fonts and sizes, Save As, and an unsaved-changes prompt, on a shared `aurorakit::text` engine.
- **Files:** drag and drop, icon and sortable list views, multi-select, search, Quick Look, picture thumbnails and a Trash with Put Back.
- **Desktop:** window animations, notifications and a Notification Center with a calendar, Spotlight (apps, indexed files, settings, calculations, commands), a system clipboard, screenshots, pictures as wallpaper, and a dynamic accent colour.
- **Settings:** Appearance, Display (live resolution changes on Bochs/QEMU VGA, boot-time modes elsewhere), Keyboard (six layouts, key repeat), Date & Time, Sound, Notifications and About.
- **Deferred:** complex-script shaping (Arabic, Indic) and a user-space font server; Sound pane controls take effect with the audio driver in M5.

## ✅ M5: Real hardware

- **Multiprocessor:** the other CPUs start through a real-mode trampoline (INIT-SIPI-SIPI); per-CPU run queues with work stealing, reschedule IPIs and TLB shootdowns. System calls copy user memory through a fault-recoverable routine, so threads can't crash the kernel by unmapping buffers.
- **Clocks:** the HPET, and the TSC calibrated against it as the clock.
- **Interrupts:** MSI and MSI-X; the AHCI, NVMe and virtio disks sleep until their completion interrupt.
- **USB:** an xHCI driver with hubs, keyboards (the shared layouts, software key repeat), mice and tablets (report descriptors), and mass storage mounted at `/Volumes` with eject. Booting from a USB stick keeps your files on it.
- **Sound:** an Intel HD Audio driver, a mixer, playback streams for programs, system sounds, the volume keys and a menu-bar volume control.
- **ACPI:** the `acpi` crate's AML interpreter on its own task (a failure there only turns ACPI off): the power button, batteries, the power adapter, the lid, and sleep states.
- **Sleep:** S3 suspend to RAM and resume, with every driver brought back and the other CPUs restarted.
- **Tools:** `lspci`, `lsusb`, `cpuinfo`, `dmesg`, `battery`, `play` and `volume`; a boot log on the EFI partition; [HARDWARE.md](HARDWARE.md) for testing on real machines.
- **Deferred:** tickless idle with TSC-deadline timers; I²C touchpads; Modern Standby (S0ix); native GPU drivers for real hardware (Intel, AMD, Nvidia modesetting, on the display core that virtio-gpu and Bochs VGA already use); per-CPU NVMe queues. (USB keyboard LEDs and the older USB controllers arrived in 0.9.)

## ✅ M6: Networking and the web

- **Adapters:** virtio-net (modern virtio-pci, MSI-X) and Intel e1000/e1000e (82540EM, 82574L and relatives; MSI or polled). Both come back after sleep. Later: Intel igb (82576, tested in QEMU; also I210/I211/I350) and igc (I225/I226), Realtek RTL8139 (tested in QEMU) and RTL8168/8111/8125; none of them yet tried on real hardware.
- **Our own IPv4 stack** in the kernel (`net/`): ARP with a pending queue, ICMP echo and unreachable, UDP, and TCP with RFC 6298 retransmission timers, slow start and congestion avoidance, fast retransmit, delayed ACKs, window scaling, zero-window probes and TIME_WAIT. A DHCP client renews its lease, and loopback works.
- **Sockets** for programs (`SOCKET`, `CONNECT`, `BIND`, `LISTEN`, `ACCEPT`, `SENDTO`, `RECVFROM`, …) with timeouts and non-blocking mode; `GETRANDOM` from a ChaCha20 generator seeded by RDSEED/RDRAND.
- **The web** (host-tested `no_std` libraries):
  - `libs/web`: URLs (RFC 3986) and HTTP/1.1 with redirects, chunked bodies, keep-alive through a connection pool, and gzip/deflate
  - `libs/tls`: TLS 1.3 (X25519 or P-256; AES-GCM or ChaCha20-Poly1305), TLS 1.2 fallback (ECDHE with AEAD ciphers, extended master secret), and our own DER/X.509 path validation against Mozilla's roots. Tested against rustls and RFC 8448.
  - `libs/surf`: the browser engine. An HTML parser, CSS (selectors with an ancestor Bloom filter, the cascade, custom properties, `calc()`, media queries, `::before`/`::after`), and layout: blocks, inline text, lists, tables, flexbox, grid, floats as rows, form controls.
- **Images:** our own JPEG (baseline and progressive) and GIF decoders, for Nebula, Preview and Files.
- **Nebula:** history, links, fragments, search forms, find in page, zoom, downloads, a start page; `.html` files open in it.
- **Tools and UI:** `ping`, `ifconfig`, `nslookup`, `fetch`; shell lists (`&&`, `||`, `;`); the menu-bar network item, Settings → Network, and a Network tab in Activity Monitor.
- **Deferred:** IPv6; cookies and POST forms; tabs; WebP and SVG; Wi-Fi and other adapters (Realtek).

## 0.7: Desktop and offline documentation

- Learn: a native, searchable English and Canadian French handbook.
- Visual MRU window switching, explicit snap states, and keyboard focus improvements.
- Conservative localized composition with reproducible release measurements.
- See [0.7.0 update notes](releases/0.7.0/en.md) and the [handbook](handbook/README.md).

## In progress: graphics for games

The direction for graphics, replacing the earlier plan to port more Linux GPU drivers: WaveOS's own Rust display drivers expose Linux's DRM/KMS interface, and unmodified Mesa runs on top through the Linux layer, with Steam games as the eventual target.

- **Done:** the display core; native virtio-gpu, Intel (Skylake to Raptor Lake) and AMD (Polaris to Navi 3x, Ryzen APUs) display drivers that take over the firmware's mode (Intel and AMD untested on real hardware); Nvidia detection; several displays; `/dev/dri/card0` with dumb buffers, atomic commits, page-flip events and PRIME (libdrm's `modetest` runs unmodified); `evdev`, `hidraw`, `uinput` and udev-style discovery (SDL2 recognises controllers unmodified); Wave Play.
- **Also done:** Mesa's software path (llvmpipe, lavapipe) over the DRM device, with OpenGL and Vulkan programs and the browsers drawing in windows without a copy, and full-screen windows scanned out directly.
- **Next:** virtio-gpu 3D (virgl and Venus); render nodes, GEM and command submission for Intel, then AMD; an Nvidia driver through the GSP firmware ([design](developer/nvidia-display.md)); 32-bit Linux programs for Steam's client.

## Done in this round: 64-bit Arm, and a faster web

- **aarch64:** the kernel, every native program and the Linux layer's arm64 ABI run on QEMU's `virt` machine, at the host processor's own speed on Apple-silicon Macs, where it is now what `make run` starts ([porting notes](developer/porting.md)). The launcher's Chipset card switches between it and x86-64; each has its own disk, and `cargo xtask migrate-disk` carries files across.
- **Browsers:** Voyage and Firefox on arm64, two to three seconds from a cold start to a painted page there, against 16 to 22 on the emulated x86-64 build on the same Mac ([measurements](LINUX.md#two-abis-x86-64-and-arm64)).
- **Sound for Linux programs** through an ALSA device layer, and video with sound in both browsers (`cargo xtask video-check`).
- **Under it:** a lost wake-up in the kernel's wait queues (disk reads and syncs that woke 5 s late), a deadlock when a window buffer was freed, AuroraFS consistency after power cuts (`cargo xtask powercut`, `cargo xtask fsck`), swap and compressed memory, per-app sandboxing, the Store and A/B system updates, network over USB.
- **Still open on aarch64:** suspend to RAM; address-space identifiers (every switch flushes the TLB today, measured at 0.5-1 µs); per-process pointer-authentication keys; a YouTube video that can stop about 30 s into its first play after a boot.

## Future work

Candidates, in no fixed order:

- Per-CPU NVMe queues.
- Bluetooth input; hibernation; webcams (UVC).
- More Linux graphics drivers on the compatibility layer in `kernel/linux` (virtio-gpu runs since 0.9): TTM, the DRM GPU scheduler and firmware loading for amdgpu and i915/xe, then a Linux-compatible interface for Mesa (3D). The Linux driver should also restore the display after sleep. (The native virtio-gpu driver, the default since the display core, does; the ported one stays selectable with `gpu=linux`.)
- Wi-Fi on real hardware: firmware loading and Intel's command protocol for the iwlwifi adapters WaveOS already recognizes (the station, WPA2 and CCMP are done); WPA3 (SAE).
- More of the web platform (WebP, SVG, CORS); a user-space window server.

## 0.8: Pulsar, JavaScript

- **The engine** (`libs/script`): lexer, parser, a compiler with static scopes, a bytecode VM whose generators and async functions suspend by saving their frames, mark-and-sweep collection, and the standard library with our own regular expressions. 31,017 test262 tests pass (88.1% of those run).
- **In Nebula:** page scripts and scripts they insert, the DOM and events, timers and animation frames, element geometry from layout, `fetch` and `XMLHttpRequest`, per-site `localStorage`, and web APIs written in JavaScript (URL, observers, AbortController). Clicks, typing and form submission go to scripts first. jQuery runs.
- **In Terminal:** `js`, with typed lines forwarded to a running program's stdin.
- **Deferred:** Proxy, BigInt, typed arrays, modules, cookies, and same-origin checks on `fetch`.

## 0.9: The web, faster and wider

- **IPv6:** neighbor discovery, SLAAC and RDNSS, ICMPv6, TCP and UDP over IPv6 with `SockAddr6` sockets, `net_info6`; AAAA lookups and Happy Eyeballs in corekit; `ping` and `ifconfig` show IPv6. Validated against QEMU's IPv6 network (the host's loopback over IPv6), not the real internet.
- **Loading:** parallel downloads of styles and scripts, paint before scripts, scripts in time slices, an HTTP cache with revalidation, a DNS cache, a frozen old page while the next loads, and coalesced layouts. Measured with `tools/benchmark_web.py`.
- **Tabs** that keep working in the background, with storage events between them.
- **Web platform:** comment nodes, TreeWalker and NodeIterator, a live MutationObserver, real getComputedStyle, `<style>` elements scripts add, a 2D canvas with paths and text, image load events.
- **Deferred:** HTTP/2, TLS session resumption, incremental layout, running page scripts off the UI thread.
