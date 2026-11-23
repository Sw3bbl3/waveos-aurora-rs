# WaveOS Aurora: the complete feature list

Everything that works today, layer by layer. For the short tour, see the [README](../README.md); for how the pieces fit, [ARCHITECTURE.md](ARCHITECTURE.md).

## What works today (Milestones 1–6)

- **Boots on UEFI x86_64** with its own bootloader (`firstlight`). It loads the kernel and a read-only system image.
- **Aster kernel** (hybrid design):
  - memory management, with per-process address spaces and no-execute pages
  - **multicore**: every CPU runs tasks, with per-CPU run queues, work stealing and TLB shootdowns
  - HPET and TSC clocks, APIC timers, MSI/MSI-X interrupts
  - `syscall`/`sysret`, with fault-safe copies of user memory
  - PS/2, VMware absolute pointer and USB input
  - **ACPI** through an AML interpreter: the power button, battery, power adapter and lid, shutdown and restart
  - **Sleep** (S3 suspend to RAM) and resume
- **Real user space.** Every app is its own **ring-3 process** with its own address space:
  - **Crash isolation:** an app that crashes gets a "quit unexpectedly" report, and everything else keeps running.
  - **System calls:** covering processes, threads, files, pipes, windows, events, the clipboard, drag and drop, notifications, preferences, clocks, sound and power. Every pointer is validated.
  - **Libraries:** the `corekit` runtime, the **AuroraKit** windowing toolkit, and **WaveUI**, the declarative interface framework every app is written with. Together with Studio, the `wavesdk` command and GINA packages they make up the **WaveSDK**.
- **Installable apps:** signed GINA packages (Ed25519 publisher keys, a SHA-256 for every file, hostile-archive checks) install for everyone (`/Applications`, administrators) or one account, atomically (an interrupted install leaves the old or the new version, never a mix), with their own icons in Shoal, Seek and the dock, file associations in Files' *Open with*, and their data kept apart. `pkg` installs from files, addresses and signed repositories, updates, verifies, searches and removes; Installer shows the publisher, signature, permissions and size first; Settings → Apps lists and removes them. `cargo xtask repo` publishes a repository; Doom and the WaveUI Gallery ship as sample packages. See the GINA section of [WAVESDK.md](WAVESDK.md).
- **Lumen Server window server**, in the kernel like Windows NT's:
  - apps draw into shared-memory surfaces that Lumen Server composites
  - rendering is damage-tracked, with anti-aliased shapes, soft shadows and frosted glass, drawn with SSE2 in the window server's own task
- **Display drivers** on a KMS-shaped display core (CRTCs, primary and cursor planes, connectors with EDID, atomic commits, refcounted buffers): the firmware framebuffer, Bochs/QEMU VGA, a native **virtio-gpu** driver that scans the compositor's buffer out without a copy, transfers only damaged rectangles, shows a hardware cursor, follows the host window and restores the picture after sleep, a native **Intel graphics** display driver (Skylake to Raptor Lake; untested on real hardware) that takes over the firmware's mode, scans the compositor's buffer out of RAM through the GGTT, adds a hardware cursor, reads EDID over GMBUS and DP AUX, detects hotplug and drives the panel backlight from the brightness keys, with DP link training and mode changes on Skylake-class GPUs behind `intel=modeset`, and a native **AMD Radeon** display driver (Polaris, Vega, RDNA 1–3 and Ryzen APUs from Raven to Phoenix; untested on real hardware) that takes over the firmware's mode, flips between buffers in video memory, adds a hardware cursor, reads DisplayPort EDID and detects hotplug. Nvidia GeForce GPUs (GTX 900 to RTX 40) are identified and stay on the firmware framebuffer. See [developer/display-core.md](developer/display-core.md), [developer/intel-display.md](developer/intel-display.md), [developer/amd-display.md](developer/amd-display.md) and [developer/nvidia-display.md](developer/nvidia-display.md).
- **The Linux DRM device**: `/dev/dri/card0` for each display, with Linux's DRM/KMS ioctls and struct layouts — mode-setting objects and properties, legacy mode sets, page flips, cursor and planes, atomic commits (test-only, nonblocking, flip events), dumb buffers mapped through the card, PRIME dma-bufs that pass between processes and map, vblank waits and events read through `poll`/`epoll` — and the `/sys/class/drm` entries libdrm enumerates devices with. A Linux program's first mode set takes the display from the desktop; the desktop comes back when it lets go, closes the card or is killed. libdrm's `modetest`, built unmodified, is included and page-flips at the display's refresh rate, with legacy calls and with atomic commits. Planes list their formats with the linear modifier (`IN_FORMATS`). See [developer/display-core.md](developer/display-core.md#the-drm-device-devdricard0).
- **Mesa, OpenGL and Vulkan**: Debian's Mesa runs unmodified, in software (llvmpipe): `kmscube` and `glmark2` full screen through GBM and EGL on `/dev/dri/card0` (OpenGL ES 3.2 and OpenGL 4.5, page-flipped with events), Vulkan 1.3 through lavapipe, and OpenGL in WaveOS windows through the Wayland bridge; `cargo xtask test --mesa` checks each, with screendumps of what reached the screen. A per-program frame summary (drawing, commit, waiting) and `WAVEOS_SYSCOUNT=1` system call counts show where frames go. See [developer/display-core.md](developer/display-core.md#mesa).
- **Several displays**, as on macOS and Windows: every connected monitor extends the desktop (or mirrors the main one), each at its own resolution and scale (a HiDPI display draws at 2× next to a 1× one) with its own wallpaper. The menu bar and Harbor stay on the main display (any display can be made the main one); windows drag across, maximise and moor on the display they're on, and open where the pointer is; the pointer crosses where displays touch. Unplug a display and its windows move to the main one; plug it back in and they return. Settings → Display arranges them by dragging (or Left/Right/Above/Below), and each monitor's arrangement comes back by its EDID identity. A slow display never holds up the others. Apps can list the displays and learn which one their window is on (`corekit::display`). Try it with `cargo xtask run --gpu virtio --displays 2`.
- **Linux graphics drivers**: a compatibility layer written for WaveOS (memory, locks, work queues, ID maps, DMA fences, virtio) runs Linux 6.12's DRM display core and its **virtio-gpu** driver unmodified; an in-kernel DRM client sets the mode and Lumen Server draws through it (`cargo xtask run --gpu virtio-linux`; the default for virtio GPUs is now WaveOS's own driver, below). Only permissively licensed Linux code is included; WaveOS replaces the GPL-only parts. See [kernel/linux](../kernel/linux/README.md).
- **Linux programs:** static Linux x86-64 programs run as they are, with Linux's system calls, a Linux-shaped file tree with symbolic links, `/proc` and `/dev`, threads, `fork`/`exec`/`wait`, signals with handlers, `poll`, `select` and `epoll`. BusyBox is included (its shell runs interactively in Terminal), and Linux programs can open WaveOS windows and play sound. Dynamically linked Debian programs, sockets and shared memory work, and `wl-bridge` shows Wayland (GTK) programs in WaveOS windows. Debian's packages, Firefox ESR among them, go on the disk with `cargo xtask run`; mapped files are read once and shared between processes. See [LINUX.md](LINUX.md).
- **Doom:** the Doom app starts doomgeneric (a Linux program) with your WAD files from Documents → Doom, or downloads Freedoom for you.
- **Boot and Shore:** a boot screen with the WayV logo, start-up progress and the version, in the Light or Dark appearance; first-start setup of the one account (its password is salted and hashed with PBKDF2-HMAC-SHA256), then login, Lock Screen (Super L) and optional automatic login.
- **The living-water desktop:**
  - a top bar of glass bubbles: the WayV menu and the app on the left; **Crest** in the middle with the time (the date on pointing, activities such as volume and new notifications), which can be turned off; **Seek** and the status ring on the right, opening **Current** (network, Calm, appearance, lock, sound, battery)
  - **Harbor**, a floating dock in three bays (pinned apps, open apps, the Downloads and Documents stacks and Trash), with a waterline under open apps, window peeks, ripples while an app starts, and sinking away from a window that reaches it
  - a permanent compact window strip reserves room for the title and smoothly revealed minimize, maximize and close controls; keyboard access with Alt+Space, dependable resize edges/corners, a focused waterline, and **Moorings** snapping into six layouts
  - **Shoal** (all apps, full screen, with search and groups), **Seek** (apps, files, settings and sums), **Drift** (calendar, Calm and notifications) and **Glide** (Alt+Tab with window pictures)
  - Manrope throughout, light and dark everywhere, broadcast live to every app, and an accent colour that can follow the wallpaper
  - six licensed natural photographs, shown faithfully in both appearances: Tide, Mist, Dunes, Ripple, Coast and Alpine
  - an optional interactive tour in Welcome and Help, with anchored guidance and temporary practice content; reduced motion, reduced transparency and increased contrast throughout shared controls
- **Apps:** Voyage, Firefox, NetSurf, Files, Terminal, Notes, Calculator, Settings, Lens, Paint, Clock, Pulse, About This Computer, Welcome, Learn, Studio, Installer and Doom.
- **Networking**, all our own:
  - **drivers** for virtio-net, Intel e1000/e1000e and igb/igc, and Realtek RTL8139 and RTL8168/8111/8125, interrupt-driven where MSI allows (see [HARDWARE.md](HARDWARE.md#wired-network-adapters) for what is tested)
  - **USB network adapters, docks and tethering**: CDC-ECM, CDC-NCM (frames aggregated into transfer blocks), CDC-EEM and RNDIS (Android USB tethering), ASIX AX88772 and AX88179, and Realtek RTL8152/8153/8156 through their CDC configuration; plugged in and out at any time (the interface and DHCP follow at once, connections through a pulled adapter fail immediately, routes move to the next interface), several at once, after sleep; Settings → Network shows each with its speed (see [HARDWARE.md](HARDWARE.md#usb-network-adapters) for what is tested)
  - an **IPv4 and IPv6 stack** in the kernel: ARP and neighbor discovery, ICMP and ICMPv6, UDP and **TCP** over both (retransmission timers, fast retransmit, congestion control, window scaling), **DHCP** and IPv6 **autoconfiguration** (SLAAC, RDNSS), and BSD-like sockets for programs
  - **DNS** asking for A and AAAA together, with bounded retries, TCP fallback for truncated replies and a cache that honours short/zero TTLs and invalidates on network changes; bounded IPv6/IPv4 connection fallback; an **HTTP/1.1** client with keep-alive, chunked and gzip responses
  - **TLS 1.3** (and 1.2 for older servers) with our own handshake, record layer and X.509 certificate checks against Mozilla's root CAs. Cryptographic primitives come from RustCrypto.
  - the network in **Current**, **Settings → Network**, and network graphs in Pulse; an on-demand **Check connection** action and `netcheck [URL]` distinguish configuration, DNS and verified HTTP/TLS access
  - **Wi-Fi**: open and WPA2-Personal networks, with our own supplicant (4-way and group key handshakes) and CCMP encryption, a Wi-Fi section in Settings → Network and wlanN interfaces. Intel adapters (7265 to AX211) are recognized but their radio can't be started yet; simulated networks for testing (`cargo xtask run --wifi-sim`)
- **Voyage**, the web browser ([details](LINUX.md#voyage)): our own browser around WebKitGTK 2.50 (WebKit, the engine of Safari: JavaScriptCore with its JIT, CSS, web fonts, media), through the Wayland bridge:
  - in the dock; web links and `.html` files open in it
  - tabs (dragged into order, shared web process), an address field that also searches (DuckDuckGo), find in page, zoom, downloads to Downloads, cookies and site data kept, a start page with a search box
  - light and dark like WaveOS (pages see `prefers-color-scheme`), a toolbar below the system's reserved window strip, the mouse's back and forward buttons
  - actionable navigation and certificate errors, Retry/Back/Stop, download progress and cancellation, unique download names, persistent cookies and site data; damage-aware Wayland buffer copying/presentation
- **Firefox**: Debian's Firefox ESR 140, unchanged, with full JavaScript, through the Wayland bridge, from the launcher or Seek ([details](LINUX.md#firefox)):
  - a clean start: no welcome tour, no window buttons of its own (WaveOS's handle has them), a compact toolbar, a new tab page with just a search box, nothing sent to Mozilla, downloads in Downloads; Chinese, Japanese and Korean text shows too
  - the dark theme when WaveOS is dark; the mouse's back and forward buttons work
- **NetSurf**, the small browser ([details](NETSURF.md)): NetSurf 3.11, built as a Linux program that draws straight into its WaveOS window, in the system image; it opens when neither Voyage nor Firefox is on the disk:
  - HTML and CSS with tables, floats and positioning; PNG, JPEG, GIF and BMP pictures; cookies
  - HTTP and HTTPS (curl with mbed TLS) against Mozilla's root certificates; sharp at 2×
  - the WaveOS clipboard, typing in any keyboard layout, and back and forward from Alt+←/→ and a mouse's side buttons
  - no JavaScript
- **Nebula Engine**, our own HTML and CSS engine, lays out Learn's handbook:
  - an HTML parser, and CSS with selectors, the cascade, custom properties, `calc()`, media queries and generated content
  - layout: blocks, inline text with line breaking, lists, tables, flexbox and grid rows, floats, form controls
  - PNG, **JPEG** (baseline and progressive) and GIF images, from our own decoders
- **Pulsar**, our JavaScript engine ([details](PULSAR.md)):
  - the language through ES2023: classes with private fields, generators, `async`/`await`, destructuring, modules aside
  - the standard library, our own regular-expression engine, and `Date` in the local time zone
  - a DOM, events, timers, `fetch` and storage for the Nebula engine, tested on the host
  - `js` in Terminal: a prompt with multi-line input, files, or `-e`
- **aurora-sh**, the Terminal shell. It runs about 35 programs from `/System/Bin` as separate processes (`ls`, `cat`, `grep`, `wc`, `ps`, `kill`, `cp`, `mv`, `neofetch`, `lspci`, `lsusb`, `cpuinfo`, `dmesg`, `battery`, `play`, `ping`, `ifconfig`, `nslookup`, `fetch`…). It supports pipelines (`ls -l | grep txt`), redirection (`>`, `>>`), lists (`a && b || c; d`), Ctrl+C and Tab completion.
- **Storage:**
  - PCI enumeration, plus interrupt-driven **AHCI** (SATA), **virtio-blk** and **NVMe** drivers
  - **SD cards and eMMC** on SD Host Controllers (PCI or ACPI): PIO, SDMA and ADMA2, high speed, UHS-I, eMMC HS200 (and HS400 on request), TRIM and cache flush, recovery from bus errors. WaveOS installs to and runs from them; eMMC boot partitions stay untouched
  - **USB sticks and SD cards** appear in Files under Locations when plugged in and can be ejected; booted from one, WaveOS keeps your files on it
  - your files live on **AuroraFS**, our own journaled filesystem, and survive reboots and power cuts
  - the EFI partition is readable and writable at `/Boot` (FAT32 with long file names)
  - `/System` is the read-only system image
- **Files**, a real file manager: icon and sortable list views, multiple selection with a rubber band, **drag and drop** (between windows, onto folders, onto Harbor), search as you type, Quick Look (Space), picture thumbnails, and a **Trash** with Put Back.
- **Lyra, our TrueType engine,** draws text at any size, with kerning and exact anti-aliasing, even in the kernel.
- **Spotlight** (Super+Space or Ctrl+Space) finds apps, files, settings and system commands, and does quick calculations.
- **Notifications:** banners, and a Notification Center with a calendar and Do Not Disturb (click the clock).
- **Smooth windows:** open, close, shrink-to-Harbor and expand animations.
- **Notes** is a proper editor: selection, the system clipboard, undo, find, fonts, Save As. **Lens** opens PNG, JPEG, GIF and BMP pictures (our own codecs), **Paint** draws and saves PNGs, **Clock** has world clocks, alarms, a stopwatch and timers, and **Pulse** shows every process, CPU, memory and disk.
- **Settings:** appearance with accent colours and any picture as wallpaper, display resolution (live on QEMU) and the arrangement of several displays, six keyboard layouts with dead keys, date and time, and more. Everything is remembered.
- **Threads** in user space, with futex-based locks, and SSE for apps.
- **USB:** xHCI (USB 3), EHCI (USB 2), OHCI and UHCI (USB 1.1) controllers, with hubs, storage and network adapters plugged in at any time.
- **Input:** keyboards in any rollover format with media keys and a shared Caps Lock light; mice with side buttons and two wheels; touchpads with tap to click, tap-and-drag, two-finger click and smooth two-finger scrolling; game controllers (Xbox, PlayStation, Switch Pro, generic) in one standard layout with rumble, hot-plugging and several at once, for apps through `corekit::gamepad` and for Linux games through `waveos.h` (Doom plays with one) or, unchanged, the Linux way.
- **Linux input interfaces:** every keyboard, mouse, touchpad and controller is a `/dev/input/event*` device (controllers also `js*`) presented as Linux's own drivers present it (`xpad`, `hid-playstation`, `hid-nintendo`), so SDL's controller database recognises them; rumble through force feedback, `/dev/hidraw*`, `/dev/uinput` (devices made by programs work in WaveOS too), sysfs, udev's records and hot-plug messages without a udev daemon, and inotify. Input reaches only the program in front. See [developer/linux-input.md](developer/linux-input.md).
- **Wave Play:** connect a controller and the desktop transforms into a console-style layout: the windows recede, the bar and Harbor slide away, the wallpaper blurs into a backdrop lit by the focused tile, and rows of games, apps and settings rise in. Everything is reachable with the controller alone (focus memory per row, repeat that speeds up, search with an on-screen strip of letters, navigation sounds and light vibration). Games open full screen from their tile; the home button brings a quick overlay (resume, home, close, volume, battery, frame rate, power) over anything. Unplug, or press View, and every window returns exactly where it was. All motion uses interruptible springs; Reduce Motion switches without it.
- **Sound:** an Intel HD Audio driver, USB Audio Class 1 and 2 speakers, headsets and microphones (over isochronous xHCI transfers), and a mixer that follows the chosen output. System sounds, a menu-bar volume control with an output switcher, volume keys (driving a USB device's own volume control), headphone detection; programs can play audio and record (`record`).
- **Laptop-ready:** a battery indicator with time remaining, low-battery warnings, the power button, and sleep from the menu, Spotlight or the lid.

## Build commands

| Command | What it does |
|---|---|
| `cargo xtask build` | Builds the bootloader, kernel and user space into `target/esp/` |
| `cargo xtask run --monitor` | Same as `run`, plus the live System Explorer in your browser |
| `cargo xtask run` | Builds, then boots `target/waveos-aurora.img` in QEMU (4 CPUs, sound, USB, network). Your files on it persist across runs and rebuilds. Options: `--disk ahci\|virtio\|nvme\|sd\|emmc` picks the controller (`sd` and `emmc` run from a card), `--net virtio\|e1000\|e1000e\|igb\|rtl8139\|usb-ecm\|usb-rndis\|none` the network card (`usb-ecm` and `usb-rndis` plug QEMU's USB network adapter into the `--usb` controller), `--usb xhci\|ehci\|ehci-ohci\|ohci\|uhci` the USB controller, `--smp N` the CPUs, `--battery` adds a laptop battery (`--battery=host` mirrors this computer's, `--battery=20,charging` fixes one), `--usb-stick` plugs in a USB stick, `--sd-card` adds a card reader with a card, `--usb-audio` a USB speaker, `--fresh-disk` starts over, `--headless`, `--gdb`, `--int` |
| `cargo xtask test` | Runs the kernel self-tests headless, booting the same disk twice to check persistence. Add `--disk` to pick the controller, `--net` the network card (`--net usb-ecm` or `usb-rndis` runs the network tests over USB, with two adapters and a hot-unplug; add `--usb ehci` for USB 2), `--smp 1` for one CPU |
| `cargo xtask web-check --output target/network-web/check` | Builds a disposable full guest and drives Voyage through local browser/transfer/TLS fixtures and a public site sample; `--local-only` omits public sites, `--connection-only` tests live link loss/recovery, `--benchmark-only` records new-process and revisit workloads |
| `cargo xtask image` | Creates `target/waveos-aurora-usb.img` (ESP plus AuroraFS), ready to `dd` onto a USB stick |
| `cargo xtask lint` | Clippy on the host libraries, kernel, bootloader and user space, each for its own target, with warnings as errors |
| `cargo test -p aurora-sdmmc -p aurora-help -p gina -p aurorafs -p fat32 -p lumen -p aurora-image -p aurora-wav -p nebula-web -p nebula-secure -p nebula-engine -p nebula-script -p nebula-media -p aurora-elf -p aurora-mutate -p xtask` | Host-side tests: filesystems (with crash recovery), the TrueType engine, the image and sound codecs, URLs and HTTP, TLS against rustls, the browser engine, the test ACPI table, and robustness tests that feed every parser thousands of corrupted inputs (`AURORA_MUTATIONS=N` runs them N times longer) |
| `cargo xtask iso` | Creates `target/waveos-aurora.iso` for a UEFI VM (requires xorriso; enable EFI) |

The kernel log streams to your terminal over the serial port. See [BUILDING.md](BUILDING.md) for debugging and troubleshooting, and [HARDWARE.md](HARDWARE.md) to try WaveOS on a real PC.

## Using the desktop

- **Shoal** (all apps): click the first icon in Harbor, or press and release the **Windows/Super** key. Start typing to search, pick a group, and press Enter to open.
- **Seek**: **Super+Space** (or Ctrl+Space), or the magnifying glass at top right. Try an app name, a file name, `12*7+1`, `dark mode` or `keyboard`.
- **Drift**: click Crest (the time at the top centre) for the calendar, Calm and notifications.
- **Current**: click the status ring at top right for the network, Calm, appearance, sound and battery.
- **Windows**:
  - hover over the permanently reserved top strip to reveal controls; drag its title area to move, double-click to maximize/restore, or use minimize, maximize and close; Alt+Space reaches the controls by keyboard
  - hold **Super** and drag anywhere in a window to move it
  - drag a window to the top of the screen to choose a Moorings layout (halves, thirds, quarters)
  - drag any edge or corner to resize; the cursor shows the direction
- **Shortcuts**:
  - `Alt+Tab` cycles windows
  - `Ctrl+W` or `Alt+F4` closes the front window
  - `Print Screen` or `Super+Shift+3` saves a screenshot to Pictures
  - in text: `Ctrl+A/C/X/V`, `Ctrl+Z` / `Ctrl+Shift+Z`, `Ctrl+F` to find, `Ctrl+S` to save
- **WayV menu** (the mark at top-left): About This Computer, Settings, Learn, Shoal, Wave Play, Lock Screen, Sleep, Restart, Shut Down.
- **Wave Play**: plug in a controller (or choose Wave Play in the WayV menu or Seek). The d-pad or left stick moves, A opens, Y searches, LB/RB change rows, Menu or the home button opens the quick overlay, View returns to the desktop. On a keyboard: arrows, Enter, Escape, Tab, Super+Escape. Without a controller at hand, Super+G connects a keyboard-played one (arrows, Enter = A, Escape = B, Home = home button; `cargo xtask run --virtual-gamepad` connects it at start-up).
- **Super+L** locks the screen. The volume keys work anywhere; Crest shows the level.
- **Voyage** and **Firefox**: Ctrl+L for the address bar, Ctrl+T for a tab, Ctrl+W to close it, Ctrl+Tab to the next, Alt+←/→ back and forward, Ctrl+R to reload, Ctrl+F to find, Ctrl +/−/0 zoom.
- **NetSurf**: click the address bar (to either side of the window handle), type an address and press Enter; Ctrl+U clears it. Alt+←/→ go back and forward, Ctrl +/−/0 zoom, the arrow keys and Page Up/Down scroll.
- **Files**: drag items onto folders, the sidebar, another Files window, a dock app or the Trash (hold Ctrl to copy). Right-click for more; Space opens Quick Look, F2 renames, Delete moves to the Trash, Ctrl+1/2 switch views, Ctrl+F searches.
- **Terminal**: try `help`, `ps`, `df`, `neofetch`, `cpuinfo`, `lsusb`, `play --tone 440 500`, `ls -l | grep txt`, `echo hi > hi.txt`, `cat hi.txt | wc`, `ls /Boot`, `open notes`, `theme dark`, `ping example.com`, `ifconfig`, `fetch -i https://example.com`.

## How it fits together

```
 UEFI firmware
   └─ firstlight (bootloader/)     GOP mode, load kernel.elf, page tables, exit boot services
        └─ Aster kernel (kernel/)    higher half at 0xffffffff80000000
             ├─ arch/     GDT/TSS per CPU, IDT, APIC, MSI vectors, SMP start-up, context switch
             ├─ mm/       frames, heap, paging, TLB shootdowns
             ├─ sched/    per-CPU run queues, kernel threads and user threads
             ├─ acpi/     tables, AML runtime (power button, battery, lid), SCI
             ├─ power/    shutdown, restart, S3 sleep and resume
             ├─ drivers/  serial, PS/2 + keyboard layouts, vmmouse, RTC, HPET, PCI, display modes,
             │            block (AHCI, virtio-blk, NVMe, SD/eMMC, GPT, MBR), USB (xHCI, EHCI, OHCI, UHCI, hubs, HID,
             │            storage), game controllers, audio (HDA, mixer),
             │            network (virtio-net, e1000)
             ├─ net/      IPv4, ARP, ICMP, UDP, TCP, DHCP, sockets
             ├─ proc/     processes, pipes, reaper
             ├─ syscall/  system call dispatch
             ├─ fs/       VFS, block cache, AuroraFS (/), FAT32 (/Boot), TarFS (/System), RamFS
             └─ gui/      Lumen Server window server, desktop shell, clipboard, drag and drop, notifications, Spotlight
                  ↕ syscalls, shared-memory surfaces
 User space (userland/)     corekit runtime · AuroraKit · WaveUI · apps · /System/Bin tools
   libs/web · libs/tls · libs/surf   HTTP, TLS 1.3/1.2 and the Nebula engine (no_std, tested on the host)
   /System/NetSurf            NetSurf, the small browser (a Linux program, see NETSURF.md)
   /Linux (on the disk)       Debian's packages: WebKitGTK (for Voyage), Firefox ESR, bash, Python (see LINUX.md)
```

The full tour, covering the boot handoff, memory layout, processes and syscalls, storage and AuroraFS's on-disk format, and rendering, is in [ARCHITECTURE.md](ARCHITECTURE.md).

