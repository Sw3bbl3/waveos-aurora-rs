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
- **Deferred:** tickless idle with TSC-deadline timers; I²C touchpads; USB keyboard LEDs; Modern Standby (S0ix); a native GPU driver; per-CPU NVMe queues.

## ✅ M6: Networking and the web

- **Adapters:** virtio-net (modern virtio-pci, MSI-X) and Intel e1000/e1000e (82540EM, 82574L and relatives; MSI or polled). Both come back after sleep.
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

## Future work

Candidates, in no fixed order:

- Tickless idle with TSC-deadline timers; per-CPU NVMe queues.
- I²C-HID touchpads; USB keyboard LEDs.
- A native GPU driver (virtio-gpu first), so the display survives sleep.
- IPv6; more of the web platform (cookies, POST, tabs, WebP, SVG).

## 0.8: Pulsar, JavaScript

- **The engine** (`libs/script`): lexer, parser, a compiler with static scopes, a bytecode VM whose generators and async functions suspend by saving their frames, mark-and-sweep collection, and the standard library with our own regular expressions. 31,017 test262 tests pass (88.1% of those run).
- **In Nebula:** page scripts and scripts they insert, the DOM and events, timers and animation frames, element geometry from layout, `fetch` and `XMLHttpRequest`, per-site `localStorage`, and web APIs written in JavaScript (URL, observers, AbortController). Clicks, typing and form submission go to scripts first. jQuery runs.
- **In Terminal:** `js`, with typed lines forwarded to a running program's stdin.
- **Deferred:** Proxy, BigInt, typed arrays, modules, cookies, and same-origin checks on `fetch`.
