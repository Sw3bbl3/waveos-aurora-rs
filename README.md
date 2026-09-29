# WaveOS Aurora

**A 64-bit operating system built from scratch in Rust, aiming to feel as intuitive as macOS and Windows.**

Every layer is original: the UEFI bootloader, the **Tide** kernel, the storage drivers, the **WaveFS** filesystem, the **Crest** window server, the desktop shell and the apps. There is no Linux, no BSD and no borrowed userland underneath.

![WaveOS Aurora in dark mode, with Preview and a notification](docs/screenshots/preview-dark.png)

| Spotlight | Notification Center |
|---|---|
| ![Spotlight finding an app, a settings pane and a picture](docs/screenshots/spotlight.png) | ![The Notification Center with its calendar](docs/screenshots/notification-center.png) |
| **Settings** | **Activity Monitor** |
| ![Settings, Appearance pane](docs/screenshots/settings.png) | ![Activity Monitor, CPU tab](docs/screenshots/activity-monitor.png) |

## System Explorer

A live, interactive map of the whole OS: [`docs/explorer/`](docs/explorer/index.html).

![Aurora System Explorer](docs/screenshots/explorer.png)

```sh
cargo xtask run --monitor     # boots WaveOS and opens http://127.0.0.1:7777
```

While WaveOS runs, the explorer streams what the kernel is doing:
- **Boot progress**, stage by stage
- **The scheduler's decisions** as a CPU timeline, one lane per task
- **Per-task and per-process CPU time**, memory, system calls, interrupts, disk I/O and open windows

Every part of the architecture map glows with its real activity. Click a part to see what it does, its live numbers and the source files behind it. Opened on its own, the page replays a recorded session instead.

## What works today (Milestones 1–4)

- **Boots on UEFI x86_64** with its own bootloader (`aurora-boot`). It loads the kernel and a read-only system image.
- **Tide kernel** (hybrid design):
  - memory management, with per-process address spaces and no-execute pages
  - ACPI, APIC timer and interrupts
  - a preemptive scheduler
  - `syscall`/`sysret`
  - PS/2 and VMware absolute pointer input
  - ACPI shutdown and restart
- **Real user space.** Every app is its own **ring-3 process** with its own address space:
  - **Crash isolation:** an app that crashes gets a "quit unexpectedly" report, and everything else keeps running.
  - **System calls:** about 50 of them, covering processes, threads, files, pipes, windows, events, the clipboard, drag and drop, notifications and preferences. Every pointer is validated.
  - **Libraries:** the `libaurora` runtime, and the **Ripple** UI toolkit.
- **Crest window server**, in the kernel like Windows NT's:
  - apps draw into shared-memory surfaces that Crest composites
  - rendering is damage-tracked, with anti-aliased shapes, soft shadows and frosted glass
- **Aurora desktop:**
  - a menu bar, dock and searchable launcher
  - windows you can drag, resize, minimize and zoom
  - light and dark mode, broadcast live to every app
  - three procedurally generated wallpapers
- **Apps:** Files, Terminal, Notes, Calculator, Settings, Preview, Paint, Clock, Activity Monitor, About, Welcome.
- **aurora-sh**, the Terminal shell. It runs about 20 programs from `/System/Bin` as separate processes (`ls`, `cat`, `grep`, `wc`, `ps`, `kill`, `cp`, `mv`, `neofetch`…). It supports pipelines (`ls -l | grep txt`), redirection (`>`, `>>`), Ctrl+C and Tab completion.
- **Storage:**
  - PCI enumeration, plus **AHCI** (SATA), **virtio-blk** and **NVMe** drivers
  - your files live on **WaveFS**, our own journaled filesystem, and survive reboots and power cuts
  - the EFI partition is readable and writable at `/Boot` (FAT32 with long file names)
  - `/System` is the read-only system image
- **Files**, a real file manager: icon and sortable list views, multiple selection with a rubber band, **drag and drop** (between windows, onto folders, onto the dock), search as you type, Quick Look (Space), picture thumbnails, and a **Trash** with Put Back.
- **Our own TrueType engine** draws text at any size, with kerning and exact anti-aliasing, even in the kernel.
- **Spotlight** (Super+Space or Ctrl+Space) finds apps, files, settings and system commands, and does quick calculations.
- **Notifications:** banners, and a Notification Center with a calendar and Do Not Disturb (click the clock).
- **Smooth windows:** open, close, minimize-to-dock and zoom animations.
- **Notes** is a proper editor: selection, the system clipboard, undo, find, fonts, Save As. **Preview** opens PNG and BMP pictures (our own codecs), **Paint** draws and saves PNGs, **Clock** has world clocks, alarms, a stopwatch and timers, and **Activity Monitor** shows every process, CPU, memory and disk.
- **Settings:** appearance with accent colours and any picture as wallpaper, display resolution (live on QEMU), six keyboard layouts with dead keys, date and time, and more. Everything is remembered.
- **Threads** in user space, with futex-based locks, and SSE for apps.

## Quick start

You need an x86_64 or Apple Silicon Mac, or a Linux machine, with:

- [rustup](https://rustup.rs). The pinned nightly toolchain installs automatically.
- QEMU, which bundles OVMF UEFI firmware:
  - macOS: `brew install qemu`
  - Debian/Ubuntu: `sudo apt install qemu-system-x86 ovmf`

```sh
git clone https://github.com/Sw3bbl3/waveos-aurora-rs.git
cd waveos-aurora-rs
cargo xtask run          # build everything and boot it in QEMU
```

The first build takes a minute or two. After that you boot straight to the desktop.

| Command | What it does |
|---|---|
| `cargo xtask build` | Builds the bootloader, kernel and user space into `target/esp/` |
| `cargo xtask run --monitor` | Same as `run`, plus the live System Explorer in your browser |
| `cargo xtask run` | Builds, then boots `target/waveos-aurora.img` in QEMU. Your files on it persist across runs and rebuilds. Options: `--disk ahci\|virtio\|nvme` picks the controller, `--fresh-disk` starts over, `--headless`, `--gdb`, `--int` |
| `cargo xtask test` | Runs the kernel self-tests headless, booting the same disk twice to check persistence. Add `--disk` to pick the controller |
| `cargo xtask image` | Creates `target/waveos-aurora-usb.img` (ESP plus WaveFS), ready to `dd` onto a USB stick |
| `cargo test -p wavefs -p fat32 -p aurora-gfx -p aurora-image` | Host-side tests: filesystems (with crash recovery), the TrueType engine and the image codecs |

The kernel log streams to your terminal over the serial port. See [docs/BUILDING.md](docs/BUILDING.md) for real hardware, debugging and troubleshooting.

### Using the desktop

- **Launcher**: click the grid icon in the dock, or press and release the **Windows/Super** key. Start typing to search, and press Enter to open.
- **Spotlight**: **Super+Space** (or Ctrl+Space). Try an app name, a file name, `12*7+1`, `dark mode` or `keyboard`.
- **Notification Center**: click the clock in the menu bar.
- **Windows**:
  - drag the title bar to move a window
  - double-click the title bar to zoom
  - drag the bottom-right corner to resize
  - traffic-light buttons close, minimize and zoom
- **Shortcuts**:
  - `Alt+Tab` cycles windows
  - `Ctrl+W` or `Alt+F4` closes the front window
  - `Print Screen` or `Super+Shift+3` saves a screenshot to Pictures
  - in text: `Ctrl+A/C/X/V`, `Ctrl+Z` / `Ctrl+Shift+Z`, `Ctrl+F` to find, `Ctrl+S` to save
- **Aurora menu** (the wave at top-left): About, Settings, Restart, Shut Down.
- **Files**: drag items onto folders, the sidebar, another Files window, a dock app or the Trash (hold Ctrl to copy). Right-click for more; Space opens Quick Look, F2 renames, Delete moves to the Trash, Ctrl+1/2 switch views, Ctrl+F searches.
- **Terminal**: try `help`, `ps`, `df`, `neofetch`, `ls -l | grep txt`, `echo hi > hi.txt`, `cat hi.txt | wc`, `ls /Boot`, `open notes`, `theme dark`.

## How it fits together

```
 UEFI firmware
   └─ aurora-boot (bootloader/)     GOP mode, load kernel.elf, page tables, exit boot services
        └─ Tide kernel (kernel/)    higher half at 0xffffffff80000000
             ├─ arch/     GDT, IDT, APIC, context switch
             ├─ mm/       frames, heap, paging
             ├─ sched/    preemptive kernel threads
             ├─ drivers/  serial, PS/2 + keyboard layouts, vmmouse, RTC, PCI, display modes, block (AHCI, virtio-blk, NVMe, GPT)
             ├─ proc/     processes, pipes, reaper
             ├─ syscall/  system call dispatch
             ├─ fs/       VFS, block cache, WaveFS (/), FAT32 (/Boot), TarFS (/System), RamFS
             └─ gui/      Crest window server, desktop shell, clipboard, drag and drop, notifications, Spotlight
                  ↕ syscalls, shared-memory surfaces
 User space (userland/)     libaurora runtime · Ripple toolkit · apps · /System/Bin tools
```

The full tour, covering the boot handoff, memory layout, processes and syscalls, storage and WaveFS's on-disk format, and rendering, is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Roadmap

| Milestone | Focus |
|---|---|
| **M1** ✅ | Boot to a graphical desktop |
| **M2** ✅ | User space: ring 3, syscalls, ELF loader, pipes, shared-memory windows, and the "Ripple" UI toolkit |
| **M3** ✅ | Storage: AHCI, virtio-blk, NVMe, a VFS, FAT32, and our own journaled **WaveFS** |
| **M4** ✅ | Apps and polish: TrueType text, Preview, Paint, Clock, Activity Monitor, drag and drop, Spotlight, notifications, animations, Settings |
| M5 | Real hardware: SMP, HPET, USB (xHCI), power management |
| M6 | Networking: virtio-net and e1000, TCP/IP, DHCP, DNS, HTTP |

Details are in [docs/ROADMAP.md](docs/ROADMAP.md).

## License

- **Code**: [MIT](LICENSE).
- **Fonts**: Inter and JetBrains Mono, both under the SIL Open Font License. See `assets/fonts/`.
- **Sample pictures** in `assets/home/Pictures` are generated by `tools/make_sample_pictures.py`.
