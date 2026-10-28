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
  - **System calls:** about 55 of them, covering processes, threads, files, pipes, windows, events, the clipboard, drag and drop, notifications, preferences, sound and power. Every pointer is validated.
  - **Libraries:** the `corekit` runtime, the **AuroraKit** windowing toolkit, and **WaveUI**, the declarative interface framework every app is written with. Together with Studio, the `wavesdk` command and GINA packages they make up the **WaveSDK**.
- **Lumen Server window server**, in the kernel like Windows NT's:
  - apps draw into shared-memory surfaces that Lumen Server composites
  - rendering is damage-tracked, with anti-aliased shapes, soft shadows and frosted glass, drawn with SSE2 in the window server's own task
- **Linux graphics drivers**: a compatibility layer written for WaveOS (memory, locks, work queues, ID maps, DMA fences, virtio) runs Linux 6.12's DRM display core and its **virtio-gpu** driver unmodified; an in-kernel DRM client sets the mode and Lumen Server draws through it (`cargo xtask run --gpu virtio`). Only permissively licensed Linux code is included; WaveOS replaces the GPL-only parts. See [kernel/linux](../kernel/linux/README.md).
- **Linux programs:** static Linux x86-64 programs run as they are, with Linux's system calls, a Linux-shaped file tree, `/proc` and `/dev`, threads and thread-local storage. BusyBox is included, and Linux programs can open WaveOS windows and play sound. See [LINUX.md](LINUX.md).
- **Doom:** the Doom app starts doomgeneric (a Linux program) with your WAD files from Documents → Doom, or downloads Freedoom for you.
- **Boot and Shore:** a boot screen with the WayV logo, start-up progress and the version, in the Light or Dark appearance; first-start setup of the one account (its password is salted and hashed with PBKDF2-HMAC-SHA256), then login, Lock Screen (Super L) and optional automatic login.
- **The living-water desktop:**
  - a top bar of glass bubbles: the WayV menu and the app on the left; **Crest** in the middle with the time (the date on pointing, activities such as volume and new notifications), which can be turned off; **Seek** and the status ring on the right, opening **Current** (network, Calm, appearance, lock, sound, battery)
  - **Harbor**, a floating dock in three bays (pinned apps, open apps, the Downloads and Documents stacks and Trash), with a waterline under open apps, window peeks, ripples while an app starts, and sinking away from a window that reaches it
  - windows without title bars: a handle appears at the top edge and rises into a pill to shrink, expand or close; the focused window has a waterline; **Moorings** snaps windows into six layouts
  - **Shoal** (all apps, full screen, with search and groups), **Seek** (apps, files, settings and sums), **Drift** (calendar, Calm and notifications) and **Glide** (Alt+Tab with window pictures)
  - Manrope throughout, light and dark everywhere, broadcast live to every app, and an accent colour that can follow the wallpaper
  - the Tide, Aurora and Dunes wallpapers, each with a day and a night picture
- **Apps:** Nebula, Files, Terminal, Notes, Calculator, Settings, Lens, Paint, Clock, Pulse, About This Computer, Welcome, Learn, Studio, Installer and Doom.
- **Networking**, all our own:
  - **drivers** for virtio-net and Intel e1000/e1000e, interrupt-driven where MSI allows
  - an **IPv4 and IPv6 stack** in the kernel: ARP and neighbor discovery, ICMP and ICMPv6, UDP and **TCP** over both (retransmission timers, fast retransmit, congestion control, window scaling), **DHCP** and IPv6 **autoconfiguration** (SLAAC, RDNSS), and BSD-like sockets for programs
  - **DNS** asking for A and AAAA together, with a cache that honours TTLs; Happy Eyeballs connections that prefer IPv6 where it's routed; an **HTTP/1.1** client with keep-alive, chunked and gzip responses
  - **TLS 1.3** (and 1.2 for older servers) with our own handshake, record layer and X.509 certificate checks against Mozilla's root CAs. Cryptographic primitives come from RustCrypto.
  - the network in **Current**, **Settings → Network**, and network graphs in Pulse
  - **Wi-Fi**: open and WPA2-Personal networks, with our own supplicant (4-way and group key handshakes) and CCMP encryption, a Wi-Fi section in Settings → Network and wlanN interfaces. Intel adapters (7265 to AX211) are recognized but their radio can't be started yet; simulated networks for testing (`cargo xtask run --wifi-sim`)
- **Nebula**, the web browser, on our own engine:
  - an HTML parser, and CSS with selectors, the cascade, custom properties, `calc()`, media queries and generated content
  - layout: blocks, inline text with line breaking, lists, tables, flexbox and grid rows, floats, form controls
  - PNG, **JPEG** (baseline and progressive) and GIF images, from our own decoders
  - history, find in page (Ctrl+F), zoom, search forms and downloads; up to eight **tabs**, which keep loading in the background
  - fast loading: stylesheets and scripts download side by side, the page shows as soon as its styles are in, scripts run in slices so the window stays responsive, and an in-memory **HTTP cache** revalidates with ETags
  - a 2D **canvas** with paths, gradients, transforms, clipping, text and images
- **Pulsar**, our JavaScript engine ([details](PULSAR.md)):
  - the language through ES2023: classes with private fields, generators, `async`/`await`, destructuring, modules aside
  - the standard library, our own regular-expression engine, and `Date` in the local time zone
  - in Nebula: the DOM (with comments, TreeWalker and a live MutationObserver), events with capture and bubbling, timers, animation frames, `fetch` and `XMLHttpRequest`, `localStorage` per site shared across tabs, `getComputedStyle` from the cascade, and scripts that load more scripts; jQuery runs
  - `js` in Terminal: a prompt with multi-line input, files, or `-e`
- **aurora-sh**, the Terminal shell. It runs about 35 programs from `/System/Bin` as separate processes (`ls`, `cat`, `grep`, `wc`, `ps`, `kill`, `cp`, `mv`, `neofetch`, `lspci`, `lsusb`, `cpuinfo`, `dmesg`, `battery`, `play`, `ping`, `ifconfig`, `nslookup`, `fetch`…). It supports pipelines (`ls -l | grep txt`), redirection (`>`, `>>`), lists (`a && b || c; d`), Ctrl+C and Tab completion.
- **Storage:**
  - PCI enumeration, plus interrupt-driven **AHCI** (SATA), **virtio-blk** and **NVMe** drivers
  - **USB sticks** appear in Files under Locations and can be ejected; booted from a USB stick, WaveOS keeps your files on it
  - your files live on **AuroraFS**, our own journaled filesystem, and survive reboots and power cuts
  - the EFI partition is readable and writable at `/Boot` (FAT32 with long file names)
  - `/System` is the read-only system image
- **Files**, a real file manager: icon and sortable list views, multiple selection with a rubber band, **drag and drop** (between windows, onto folders, onto Harbor), search as you type, Quick Look (Space), picture thumbnails, and a **Trash** with Put Back.
- **Lyra, our TrueType engine,** draws text at any size, with kerning and exact anti-aliasing, even in the kernel.
- **Spotlight** (Super+Space or Ctrl+Space) finds apps, files, settings and system commands, and does quick calculations.
- **Notifications:** banners, and a Notification Center with a calendar and Do Not Disturb (click the clock).
- **Smooth windows:** open, close, shrink-to-Harbor and expand animations.
- **Notes** is a proper editor: selection, the system clipboard, undo, find, fonts, Save As. **Lens** opens PNG, JPEG, GIF and BMP pictures (our own codecs), **Paint** draws and saves PNGs, **Clock** has world clocks, alarms, a stopwatch and timers, and **Pulse** shows every process, CPU, memory and disk.
- **Settings:** appearance with accent colours and any picture as wallpaper, display resolution (live on QEMU), six keyboard layouts with dead keys, date and time, and more. Everything is remembered.
- **Threads** in user space, with futex-based locks, and SSE for apps.
- **USB:** xHCI (USB 3), EHCI (USB 2), OHCI and UHCI (USB 1.1) controllers, with hubs and storage plugged in at any time.
- **Input:** keyboards in any rollover format with media keys and a shared Caps Lock light; mice with side buttons and two wheels; touchpads with tap to click, tap-and-drag, two-finger click and smooth two-finger scrolling; game controllers (Xbox, PlayStation, Switch Pro, generic) in one standard layout with rumble, for apps through `corekit::gamepad`.
- **Sound:** an Intel HD Audio driver and a mixer. System sounds, a menu-bar volume control, volume keys, headphone detection, and programs can play audio.
- **Laptop-ready:** a battery indicator with time remaining, low-battery warnings, the power button, and sleep from the menu, Spotlight or the lid.

## Build commands

| Command | What it does |
|---|---|
| `cargo xtask build` | Builds the bootloader, kernel and user space into `target/esp/` |
| `cargo xtask run --monitor` | Same as `run`, plus the live System Explorer in your browser |
| `cargo xtask run` | Builds, then boots `target/waveos-aurora.img` in QEMU (4 CPUs, sound, USB, network). Your files on it persist across runs and rebuilds. Options: `--disk ahci\|virtio\|nvme` picks the controller, `--net virtio\|e1000\|e1000e\|none` the network card, `--usb xhci\|ehci\|ehci-ohci\|ohci\|uhci` the USB controller, `--smp N` the CPUs, `--battery` adds a laptop battery (`--battery=host` mirrors this computer's, `--battery=20,charging` fixes one), `--usb-stick` plugs in a USB stick, `--fresh-disk` starts over, `--headless`, `--gdb`, `--int` |
| `cargo xtask test` | Runs the kernel self-tests headless, booting the same disk twice to check persistence. Add `--disk` to pick the controller, `--net` the network card, `--smp 1` for one CPU |
| `cargo xtask image` | Creates `target/waveos-aurora-usb.img` (ESP plus AuroraFS), ready to `dd` onto a USB stick |
| `cargo xtask lint` | Clippy on the host libraries, kernel, bootloader and user space, each for its own target, with warnings as errors |
| `cargo test -p aurora-help -p gina -p aurorafs -p fat32 -p lumen -p aurora-image -p aurora-wav -p nebula-web -p nebula-secure -p nebula-engine -p nebula-script -p nebula-media -p aurora-elf -p aurora-mutate -p xtask` | Host-side tests: filesystems (with crash recovery), the TrueType engine, the image and sound codecs, URLs and HTTP, TLS against rustls, the browser engine, the test ACPI table, and robustness tests that feed every parser thousands of corrupted inputs (`AURORA_MUTATIONS=N` runs them N times longer) |
| `cargo xtask iso` | Creates `target/waveos-aurora.iso` for a UEFI VM (requires xorriso; enable EFI) |

The kernel log streams to your terminal over the serial port. See [BUILDING.md](BUILDING.md) for debugging and troubleshooting, and [HARDWARE.md](HARDWARE.md) to try WaveOS on a real PC.

## Using the desktop

- **Shoal** (all apps): click the first icon in Harbor, or press and release the **Windows/Super** key. Start typing to search, pick a group, and press Enter to open.
- **Seek**: **Super+Space** (or Ctrl+Space), or the magnifying glass at top right. Try an app name, a file name, `12*7+1`, `dark mode` or `keyboard`.
- **Drift**: click Crest (the time at the top centre) for the calendar, Calm and notifications.
- **Current**: click the status ring at top right for the network, Calm, appearance, sound and battery.
- **Windows**:
  - bring the pointer to a window's top edge: its handle rises into a pill; drag the pill to move the window, double-click it to expand, or use its buttons to shrink, expand or close
  - hold **Super** and drag anywhere in a window to move it
  - drag a window to the top of the screen to choose a Moorings layout (halves, thirds, quarters)
  - drag the bottom-right corner to resize
- **Shortcuts**:
  - `Alt+Tab` cycles windows
  - `Ctrl+W` or `Alt+F4` closes the front window
  - `Print Screen` or `Super+Shift+3` saves a screenshot to Pictures
  - in text: `Ctrl+A/C/X/V`, `Ctrl+Z` / `Ctrl+Shift+Z`, `Ctrl+F` to find, `Ctrl+S` to save
- **WayV menu** (the mark at top-left): About This Computer, Settings, Learn, Shoal, Lock Screen, Sleep, Restart, Shut Down.
- **Super+L** locks the screen. The volume keys work anywhere; Crest shows the level.
- **Nebula**: type an address or a search in the address bar (Ctrl+L). Alt+←/→ go back and forward, Ctrl+R reloads, Ctrl+F finds on the page, Ctrl +/−/0 zoom, Space and Page Up/Down scroll. `.html` files open in Nebula too.
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
             │            block (AHCI, virtio-blk, NVMe, GPT, MBR), USB (xHCI, EHCI, OHCI, UHCI, hubs, HID,
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
```

The full tour, covering the boot handoff, memory layout, processes and syscalls, storage and AuroraFS's on-disk format, and rendering, is in [ARCHITECTURE.md](ARCHITECTURE.md).

