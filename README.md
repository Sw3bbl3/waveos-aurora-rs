# WaveOS Aurora

**A 64-bit operating system built from scratch in Rust, aiming to feel as intuitive as macOS and Windows.**

Every layer is original: the UEFI bootloader, the **Tide** kernel, the **Crest** compositor, the desktop shell and the apps. There is no Linux, no BSD and no borrowed userland underneath.

![WaveOS Aurora desktop](docs/screenshots/welcome.png)

| Launcher | Dark mode |
|---|---|
| ![Launcher](docs/screenshots/launcher.png) | ![Dark mode with Files and Terminal](docs/screenshots/dark-mode.png) |

## What works today (Milestones 1 and 2)

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
  - **System calls:** about 37 of them, covering processes, files, pipes, windows and events. Every pointer is validated.
  - **Libraries:** the `libaurora` runtime, and the **Ripple** UI toolkit.
- **Crest window server**, in the kernel like Windows NT's:
  - apps draw into shared-memory surfaces that Crest composites
  - rendering is damage-tracked, with anti-aliased shapes, soft shadows and frosted glass
- **Aurora desktop:**
  - a menu bar, dock and searchable launcher
  - windows you can drag, resize, minimize and zoom
  - light and dark mode, broadcast live to every app
  - three procedurally generated wallpapers
- **Apps:** Files, Terminal, Notes, Calculator, Settings, About, Welcome.
- **aurora-sh**, the Terminal shell. It runs about 20 programs from `/System/Bin` as separate processes (`ls`, `cat`, `grep`, `wc`, `ps`, `kill`, `cp`, `mv`, `neofetch`…). It supports pipelines (`ls -l | grep txt`), redirection (`>`, `>>`), Ctrl+C and Tab completion.
- **VFS:**
  - `/System` is the read-only system image
  - `/` holds your files, in memory for now; persistent storage arrives in Milestone 3

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
| `cargo xtask build` | Builds the bootloader and kernel into `target/esp/`, an EFI System Partition directory |
| `cargo xtask run` | Builds, then boots in QEMU. Add `--headless` for no window, `--gdb` to wait for a debugger, `--int` to log interrupts |
| `cargo xtask test` | Boots the kernel's self-test suite headless and reports pass or fail |
| `cargo xtask image` | Creates `target/waveos-aurora.img`, a GPT disk with a FAT32 ESP, ready to `dd` onto a USB stick |

The kernel log streams to your terminal over the serial port. See [docs/BUILDING.md](docs/BUILDING.md) for real hardware, debugging and troubleshooting.

### Using the desktop

- **Launcher**: click the grid icon in the dock, or press the **Windows/Super** key. Start typing to search, and press Enter to open.
- **Windows**:
  - drag the title bar to move a window
  - double-click the title bar to zoom
  - drag the bottom-right corner to resize
  - traffic-light buttons close, minimize and zoom
- **Shortcuts**:
  - `Alt+Tab` cycles windows
  - `Ctrl+W` or `Alt+F4` closes the front window
  - `Ctrl+S` saves in Notes
- **Aurora menu** (the wave at top-left): About, Settings, Restart, Shut Down.
- **Terminal**: try `help`, `ps`, `neofetch`, `ls -l | grep txt`, `echo hi > hi.txt`, `cat hi.txt | wc`, `open notes`, `theme dark`.

## How it fits together

```
 UEFI firmware
   └─ aurora-boot (bootloader/)     GOP mode, load kernel.elf, page tables, exit boot services
        └─ Tide kernel (kernel/)    higher half at 0xffffffff80000000
             ├─ arch/     GDT, IDT, APIC, context switch
             ├─ mm/       frames, heap, paging
             ├─ sched/    preemptive kernel threads
             ├─ drivers/  serial, PS/2, vmmouse, RTC, input queue
             ├─ proc/     processes, pipes, reaper
             ├─ syscall/  system call dispatch
             ├─ fs/       VFS, RamFS, TarFS (/System)
             └─ gui/      Crest window server, desktop shell
                  ↕ syscalls, shared-memory surfaces
 User space (userland/)     libaurora runtime · Ripple toolkit · apps · /System/Bin tools
```

The full tour, covering the boot handoff, memory layout, interrupt routing, scheduling and rendering, is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Roadmap

| Milestone | Focus |
|---|---|
| **M1** ✅ | Boot to a graphical desktop |
| **M2** ✅ | User space: ring 3, syscalls, ELF loader, pipes, shared-memory windows, and the "Ripple" UI toolkit |
| M3 | Storage: virtio-blk, AHCI, NVMe, a VFS, FAT32, and our own **WaveFS** |
| M4 | More apps: a richer editor, an image viewer, more Settings |
| M5 | Real hardware: SMP, HPET, USB (xHCI), power management |
| M6 | Networking: virtio-net and e1000, TCP/IP, DHCP, DNS, HTTP |

Details are in [docs/ROADMAP.md](docs/ROADMAP.md).

## License

- **Code**: [MIT](LICENSE).
- **Fonts**: Inter and JetBrains Mono, both under the SIL Open Font License. See `assets/fonts/`.
