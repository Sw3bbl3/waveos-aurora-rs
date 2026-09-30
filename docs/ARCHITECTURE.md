# WaveOS Aurora architecture

This document is for someone about to read or change the code. It follows the machine from power-on to the desktop, then covers each subsystem.

## Repository layout

| Path | Crate | Target | Role |
|---|---|---|---|
| `bootloader/` | `aurora-boot` | `x86_64-unknown-uefi` | UEFI application: loads the kernel and the system image |
| `kernel/` | `tide` | `x86_64-unknown-none` | The kernel and the Crest window server |
| `libs/bootinfo/` | `bootinfo` | both | `#[repr(C)]` handoff contract |
| `libs/abi/` | `aurora-abi` | kernel + user | System call numbers, errors, `#[repr(C)]` structs, key types |
| `libs/gfx/` | `aurora-gfx` | kernel + user | Rasterizer, our TrueType engine, theme, icons, widgets, wallpapers |
| `libs/image/` | `aurora-image` | kernel + user + host | PNG decoder and encoder; JPEG, GIF and BMP decoders |
| `libs/elf/` | `aurora-elf` | bootloader + kernel | Overflow-checked ELF64 reader |
| `libs/wavefs/` | `wavefs` | kernel + host | The WaveFS filesystem engine (also used by `mkfs` in xtask) |
| `libs/fat32/` | `fat32` | kernel | FAT32 with long file names |
| `libs/wav/` | `aurora-wav` | kernel + user + host | WAV decoding (any rate → 48 kHz stereo) and encoding |
| `libs/web/` | `aurora-web` | user + host | URLs, and an HTTP/1.1 client over any byte stream (keep-alive, chunked, gzip) |
| `libs/tls/` | `aurora-tls` | user + host | TLS 1.3/1.2 client, DER and X.509 path validation |
| `libs/surf/` | `aurora-surf` | user + host | The browser engine: HTML, CSS, style and layout to a display list |
| `userland/libaurora/` | `aurora` | user | Runtime: entry point, heap, `print!`, files, processes, threads, clipboard, drag and drop, preferences |
| `userland/ripple/` | `ripple` | user | UI toolkit: windows, event loop, the `App` trait, the `text` editing engine |
| `userland/apps/*` | `app-*` | user | Surf, Files, Terminal, Notes, Calculator, Settings, Preview, Paint, Clock, Activity Monitor, About, Welcome |
| `userland/bin/*` | — | user | Command-line tools (`coreutils`), `usertest`, `crashtest` |
| `xtask/` | `xtask` | host | Build, run, image and test orchestration; system sounds, Mozilla's root CAs (from `webpki-roots`), a test SSDT (battery, lid) and a USB-stick image |
| `assets/fonts/` | — | — | Inter and JetBrains Mono, shipped in `/System/Fonts` |
| `assets/home/` | — | — | Default folders, documents and pictures for a new home volume |
| `tools/qmp.py` | — | host | Scripted clicks, drags, typing and screenshots for a running VM |
| `tools/make_sample_pictures.py` | — | host | Generates the sample pictures (procedural) |

The root workspace's `default-members` is only `xtask`, so a plain `cargo build` never tries to build the kernel for your host. `userland/` is a separate workspace. `xtask` builds it for our own target, `userland/x86_64-aurora-user.json` (ring 3 with SSE2; the kernel stays soft-float), rebuilding `core` and `alloc` with `-Zbuild-std`, links it with `userland/libaurora/user.ld` (base `0x40_0000`), and packs the binaries and fonts into `system.tar`:
- `app-*` binaries become `/System/Apps/<Name>.elf`
- everything else becomes `/System/Bin/<name>`

## 1. Boot: `aurora-boot`

`bootloader/src/main.rs` runs as a UEFI application (`EFI/BOOT/BOOTX64.EFI`):

1. **Graphics.** It opens the Graphics Output Protocol and picks 1280×800 if the firmware offers it. Otherwise it picks the largest mode no wider than 1920 pixels.
2. **ACPI.** It finds the RSDP in the UEFI configuration table.
3. **Kernel.** It reads `\aurora\kernel.elf` from the boot volume. Our own ELF reader (`libs/elf`) walks the `PT_LOAD` segments. They are copied into one physically contiguous allocation, with `.bss` zeroed.
4. **Page tables** (`paging.rs`), a fresh 4-level hierarchy with three mappings:
   - all RAM, the framebuffer and the low 4 GiB (APIC MMIO) at **`PHYS_OFFSET = 0xffff_8000_0000_0000`**, using 2 MiB pages
   - the same range identity-mapped, so the loader survives the CR3 switch
   - the kernel image at its link address **`0xffff_ffff_8000_0000`**, using 4 KiB pages
5. **System image.** It loads `\\aurora\\system.tar` (the read-only system image) into memory and records it in `BootInfo` (v2).
6. **Handoff.** It allocates a 512 KiB kernel stack and a `BootInfo` page, then calls `ExitBootServices`. The UEFI memory map is translated into `bootinfo::MemoryRegion`s and adjacent regions are merged. Then it runs `cli; mov cr3; mov rsp; call _start` with `rdi = &BootInfo`.

Everything the loader allocates is `LOADER_DATA`. That memory is reported as `MemoryKind::Bootloader`, so the kernel never hands it out.

## 2. Kernel bring-up (`kernel/src/main.rs`)

```
serial → GDT/TSS → IDT → syscall MSRs → FPU → mm (frames, heap, drop identity map, vmm)
      → ACPI tables → HPET → TSC calibration → APIC → per-CPU data → PS/2 + IRQ routing
      → VFS (RamFS at /, TarFS at /System) → scheduler → reaper → sti
      → start the other CPUs (trampoline, INIT-SIPI-SIPI)
      → PCI → display → block drivers + GPT (MSI/MSI-X) → mount WaveFS at /, FAT32 at /Boot
      → USB (xHCI) → sound (HDA + mixer) → ACPI runtime (AML task)
      → flusher → spawn "crest" (restores settings, launches Welcome) → idle
```

### Memory layout

| Virtual range | Contents |
|---|---|
| `0x0000_0000_0040_0000 …` | user program (per process) |
| `0x0000_0100_0000_0000 …` | user `mmap` region: heap arenas, window surfaces |
| `…0x0000_7fff_f000_0000` | user stack (1 MiB, guard page below) |
| `0xffff_8000_0000_0000 + phys` | physical memory window (`mm::phys_to_virt`) |
| `0xffff_c000_0000_0000 …` | kernel mappings of window surfaces (`vmm::kmap`) |
| `0xffff_ffff_8000_0000 …` | kernel image (`kernel/linker.ld`) |

- **Frames** (`mm/frame.rs`): one bit per 4 KiB frame, covering the usable regions above 1 MiB. The bitmap is carved from the first region that is large enough. Frames can be allocated singly (next-fit) or as a contiguous run.
- **Heap** (`mm/heap.rs`): a quarter of RAM, clamped to between 16 and 128 MiB. It is taken as one contiguous run and used through the physical window, so it needs no extra mappings. It is managed by `linked_list_allocator`, with interrupts disabled around each allocation.
- **Paging** (`mm/paging.rs`): removes the identity map. It also provides `map_mmio` for devices beyond the physical window.

### Clocks

- **HPET** (`drivers/hpet.rs`), found through the ACPI `HPET` table, is the reference clock.
- **TSC.** When it runs at a constant rate (invariant TSC, or under a hypervisor), it is calibrated against the HPET (or PIT channel 2) and becomes the clock: `time::now_ns()` is a `rdtsc` and a multiply. Otherwise the HPET counter is the clock. An offset keeps time monotonic across sleep, when the counters may restart.
- **Local APIC timers** are calibrated against the HPET and tick every CPU at **1 kHz**.

### Interrupts

- **Exceptions.** All exceptions have handlers. Double faults, page faults and NMIs run on their own IST stacks, so a kernel stack overflow still produces a readable crash screen. A kernel page fault inside the user-copy routine is not a crash: it resumes at the routine's error exit (`syscall/user.rs`).
- **Legacy PIC.** It is remapped to vectors `0xE0`–`0xEF` and fully masked.
- **I/O APIC.** It routes ISA IRQ1 (keyboard, vector 33) and IRQ12 (mouse, vector 44) and the ACPI SCI, honouring the MADT interrupt source overrides (polarity, trigger mode).
- **MSI and MSI-X** (`arch/irq.rs`, `pci::Device::enable_msi`). 64 vectors from `0x50` are handed out to devices, each with a handler and argument. Disk, USB and sound controllers use them; a driver whose device has neither falls back to polling.
- **IPIs.** Reschedule (`0xF0`), TLB shootdown (`0xF1`) and halt-on-panic (`0xF2`).

### Multiprocessing (`arch/smp.rs`, `arch/percpu.rs`)

- **Start-up.** The bootloader reserves a page below 1 MiB. `smp::start_aps` copies a 16→32→64-bit trampoline there, with a small page table (low memory identity-mapped plus the kernel half), and wakes each CPU from the MADT with INIT-SIPI-SIPI. Each gets its own TSS and IST stacks, loads the shared IDT, `syscall` MSRs and FPU setup, starts its APIC timer and joins the scheduler with its own idle task.
- **Which CPU am I?** Every CPU loads its own TSS selector, so `str` answers anywhere, even in an exception handler. GS is only swapped for a few instructions in the `syscall` entry stub, to find this CPU's kernel stack.
- **TLB shootdowns.** `vmm::kunmap` and unmapping a multi-threaded address space ask every CPU using those page tables to flush, and wait (servicing their own requests meanwhile).

### Scheduling (`sched/`)

- **Model.** Tasks are kernel threads (128 KiB stacks) and the threads of user processes. Each CPU has a run queue and an idle task (the context that brought it up).
- **Policy.** Round-robin with a **10 ms quantum**, preempted from each CPU's timer. New tasks go to the least-loaded CPU; woken ones prefer their last CPU, or an idle one, which is kicked with an IPI. An idle CPU steals from the busiest queue.
- **Switching** is split: under the scheduler lock a CPU picks the next task and records the switch; then it swaps stacks. A task leaving a CPU keeps its `on_cpu` flag until that CPU has switched away (`finish_switch`), and another CPU only reads its saved stack pointer after taking the flag. Dead tasks are freed after the switch.
- **Waiting.** `wait_until(timeout, condition)` marks the task sleeping *before* testing the condition, so a wakeup from another CPU is never lost. `sync::WaitQueue` builds on it for devices: a driver's interrupt handler wakes the task waiting for its completion.
- **Pinning and parking.** `spawn_on_bsp` keeps a task on CPU 0 (sleep must start there). `park_aps` moves all work to CPU 0 and halts the others before sleep.
- **Locking rule.** A spinlock held across a preemption deadlocks, so shared state uses `sync::IrqMutex`, which keeps interrupts off while held. Beware `while let Some(x) = q.lock().pop()`: the guard lives for the whole loop body. Longer critical sections that may block use `sync::Mutex`, which yields.

### Input

The IRQ handlers decode input straight into a fixed-size, allocation-free ring buffer (`drivers/input.rs`):

- **Keyboard.** Scancode set 1 is translated to USB HID usages (`drivers/keyboard.rs`), and the chosen layout turns usages into characters (`drivers/keymap.rs`: U.S., British, German, French, Spanish, Swedish, with AltGr and dead keys). USB keyboards report usages directly and share the same tables; their key repeat is done in software.
- **USB pointers.** Mice, tablets and VM pointers are read through their HID report descriptors (see below).
- **Mouse, emulated.** Under QEMU and VMware, the **vmmouse** backdoor gives an absolute pointer, so the guest cursor tracks the host cursor without grabbing it.
- **Mouse, real hardware.** Standard PS/2 relative packets, with IntelliMouse wheel support.

## 3. User space (`proc/`, `syscall/`, `mm/vmm.rs`)

### Address spaces

Every process has its own PML4:
- **Lower half:** private to the process.
- **Upper half:** the kernel. At boot, `vmm::init` pre-populates all 256 kernel PML4 slots, so every later kernel mapping lands in tables that every address space already shares.
- **Permissions:** user pages carry the USER bit, and non-code pages are marked no-execute (EFER.NXE is enabled).
- **Pointer validation:** `AddressSpace::check` walks the page tables. Every user pointer passed to a syscall is validated this way (`syscall/user.rs`), so a bad pointer returns `EFAULT` instead of crashing the kernel.

### Processes

`proc::spawn(path, argv, stdio, parent)`:
1. Reads the ELF from the VFS and maps its `PT_LOAD` segments with the right permissions.
2. Builds a 1 MiB stack with an argument block, and creates a task.
3. The task drops to ring 3 with `iretq`.

There is no `fork`. The model is spawn-style, like Windows' CreateProcess.

**Threads.** A process can run several threads (`thread_spawn`/`thread_exit`): tasks that share its address space and handles. `futex_wait`/`futex_wake` put contended threads to sleep, keyed by (address space, address); libaurora builds `sync::Mutex`, `Condvar` and `thread::spawn`/`join` on them. `thread_exit` can store 1 into a word and wake it, which is how `join` knows the thread has left its stack.

**SIMD state.** User code uses SSE2; the kernel is soft-float and never touches those registers. So only user tasks carry an FXSAVE area, saved and restored eagerly on every switch between them (`arch/fpu.rs`).

Each process has an fd table of `Handle`s:
- open files
- pipe ends (bounded 64 KiB queues)
- the log console (the default stdio)

**Scheduler integration.** Tasks carry their process's CR3 and kernel stack. On every switch the scheduler:
- loads the new CR3 when the process changes
- points `TSS.rsp0` and the syscall stack at the next task's kernel stack

**Syscall entry** (`arch/syscall.rs`, `syscall`/`sysret`):
1. The stub switches to the task's kernel stack.
2. It saves a `SyscallFrame`, enables interrupts, and calls `syscall::dispatch`.

Syscall numbers and structs live in `libs/abi`. They cover processes, memory, files, pipes, system info, windows and events.

**Exit and crashes:**
- **Termination is cooperative for every thread.** `exit`, a crash or `kill` records the exit code (the first one wins) and flags the process; each thread leaves at its next safe point (syscall return, a timer tick in ring 3, or a blocking wait), so none dies while holding a kernel lock.
- **CPU faults in ring 3** end only the faulting process (`proc::crash_current`), and the desktop shows a Problem Report.
- **The `reaper` kernel task** frees a process once the scheduler has retired its last thread: it closes handles (so pipe peers see EOF), closes windows, and drops the address space.

### libaurora and Ripple
- **libaurora:** the runtime. It provides `_start`, a heap that grows by `mmap`-ing new arenas, and `print!`, which formats into one buffer and writes it once. It also has file, process and time APIs.
- **Ripple:** the UI toolkit. An app implements `ripple::App` (draw/key/click/drag/hover/scroll/tick, drop targets, close requests) and calls `ripple::run`. Ripple creates the window, draws the app with `aurora-gfx` into the shared surface, calls `win_present`, and turns window-server events into method calls.
- **`ripple::text`:** one editing engine for every text box: `TextEdit` (caret, selection, word and line motion, undo grouped by typing bursts, the clipboard), a word-wrapping `TextView` with mouse selection (double-click a word, triple-click a line, drag with auto-scroll), and a single-line `TextField`.

## 4. Crest window server (`kernel/src/gui/`)

- **Main loop.** Crest is the `crest` kernel task (`gui/mod.rs`). Each frame it:
  1. drains input
  2. applies client commands (`server::take_commands`: window created, closed, presented, retitled; open app/file; theme; power) and crash reports
  3. repaints only the **damaged rectangles** into a RAM back buffer
  4. copies them to the GOP framebuffer
- **Client windows** (`server.rs`, `apps/client.rs`):
  - **Surface:** each window owned by a process has a surface, which is frames mapped both into the kernel (`vmm::kmap`) and into the process (`map_shared`).
  - **Compositing:** `ClientApp` adapts it to the window manager. It blits the surface with rounded bottom corners, and turns clicks, keys, scrolls, focus and resizes into `aurora_abi::Event`s on the window's queue.
  - **Events:** the process receives events through the blocking `next_event` syscall.
- **Built-in dialogs.** A few system dialogs (Power, Problem Report, "Keep this resolution?") are built into the server and implement the kernel-side `App` trait directly.
- **Rendering primitives.** Rendering is `aurora-gfx`:
  - integer anti-aliased shapes, SDF shadows and frosted glass
  - glass samples a wallpaper blurred once at startup
  - bilinear image scaling with correct transparent edges (`draw_image`)
  - wallpapers are procedural, or any picture
- **Text** (`libs/gfx/src/ttf`, `font.rs`) is our own TrueType engine. It reads `cmap`, `glyf` outlines (simple and composite), `hmtx`, OS/2 metrics and pair kerning (`kern` and GPOS), and rasterizes by exact signed-area accumulation in 16.16 fixed point, so the soft-float kernel can use it too. Glyphs are cached per face, size and quarter-pixel offset. Crest and every app install the faces from `/System/Fonts`; a few build-time sizes cover the time before that (and the panic screen).
- **Desktop** (`desktop/mod.rs`): the window stack, focus, drag and resize, zoom and minimize. A press in a window's content captures the pointer until release, so apps get drags even outside their window.
- **Animations** (`desktop/anim.rs`): opening, closing, minimizing into the dock, restoring and zooming draw a snapshot of the window scaled and faded between two rectangles while the real window stays hidden. Client windows appear once they've drawn their first frame. Crest runs at 60 Hz only while something moves.
- **Shell** (`desktop/shell.rs`): the menu bar, dock (with the Trash), launcher and menus. The catalog in `apps/mod.rs` maps app names and icons to program paths.
- **Clipboard** (`clipboard.rs`): one item, text or a list of files; files paste as their paths where text is wanted.
- **Drag and drop** (`dnd.rs`, `desktop/dragdrop.rs`): an app calls `drag_start` during a press; Crest takes the pointer, draws the drag image, sends `DRAG_OVER`/`DRAG_LEAVE` to windows underneath, and on release delivers `DROP` (the target fetches the payload with `drag_data`) and `DRAG_END` to the source. Dock apps open dropped files, and the dock Trash takes them.
- **Notifications** (`notify.rs`, `desktop/notifications.rs`): `notify` from apps, or the system; banners slide in at the top right, and the Notification Center (click the clock) keeps the history next to a calendar and Do Not Disturb.
- **Spotlight** (`desktop/spotlight.rs`, `fs/index.rs`): apps, files, settings panes, calculations and commands. The file index is built by a background task at boot and kept current by hooks in the VFS (create, mkdir, rename, unlink).
- **Preferences** (`settings.rs`, `prefs.rs`): `key=value` lines in `/Settings/aurora.conf`, read and written with `pref_get`/`pref_set` and applied at once: dark mode, the accent colour, keyboard layout and key repeat, clock format, Do Not Disturb, volume.
- **Display modes** (`drivers/display.rs`): on Bochs/QEMU VGA the resolution switches live (new framebuffer and back buffer, every window clamped, a `SCREEN` event to apps, then a 15-second "Keep this resolution?"). Elsewhere the firmware's GOP modes (passed in BootInfo v3) are offered and the choice goes to `\aurora\boot.conf` for the bootloader.
- **Screenshots:** Print Screen or Super+Shift+3 saves a PNG to Pictures, encoded on a background task.

## 5. Storage (`drivers/pci.rs`, `drivers/block/`, `fs/`)

**PCI.**
- Enumerates devices through ECAM, using the base from the ACPI MCFG table, with the legacy `0xCF8` ports as a fallback.
- Sizes BARs (including 64-bit BARs) and walks capability lists.
- BARs above the physical window are mapped uncached with `paging::map_mmio`.

**Block layer.** Drivers implement `BlockDevice` (sector size and count, read, write, flush) and register their disks. `block::init` then reads each disk's GPT and registers the partitions as devices of their own. All drivers are **polled**: they copy through physically contiguous DMA bounce buffers (`Dma`) and yield to the scheduler while waiting. Interrupt-driven I/O comes with MSI in M5.

| Driver | Details |
|---|---|
| `ahci.rs` | Resets the HBA, then for each port: FIS receive, COMRESET, and one command slot. Uses IDENTIFY, READ/WRITE DMA EXT and FLUSH CACHE EXT. Empty ports are skipped in about 20 ms. |
| `virtio.rs` | Modern virtio-pci 1.0 (common, notify and device-config capabilities). One split virtqueue with 16 entries; each request is a 3-descriptor chain (header, data, status). |
| `nvme.rs` | Resets the controller, then sets up the admin queue, Identify controller/namespace, and one I/O queue pair. Transfers use PRP1 and PRP2, or a PRP list above 8 KiB. Supports 512 B and 4 KiB LBA formats. |

**VFS mounts.** The longest matching prefix wins:

| Path | Filesystem | Notes |
|---|---|---|
| `/` | WaveFS on the partition typed `57415645-4653-4175-726F-72612D465331` | Falls back to RamFS when no disk is found |
| `/System` | TarFS over the boot-time `system.tar` | Read-only |
| `/Boot` | FAT32 on the EFI System Partition | Read/write, long file names, case-insensitive |

Disk-backed filesystems sit on `fs/cache.rs`, a write-back cache of 4 KiB blocks with LRU eviction. Its `flush` writes dirty blocks in order, merging adjacent runs, and then flushes the device. A `flusher` kernel task calls `sync_all` every 5 s, and shutdown and restart sync first.

### WaveFS on-disk format (`libs/wavefs`)

```
block 0            superblock (magic WAVEFS01, geometry, counts, UUID, label, CRC32); backup in the last block
journal            1/16 of the volume, 32–1024 blocks
block bitmap       1 bit per 4 KiB block
inode bitmap       1 bit per inode (one inode per 16 KiB of disk)
inode table        256-byte inodes: kind, links, size, times, 12 extents + 1 overflow extent block
data               file contents; directories are files of (inode, kind, name) records
```

**Consistency.** Every metadata change (superblock, bitmaps, inodes, directory contents) goes into the running transaction. The transaction is an overlay of block images that reads also see. `commit` then:
1. Flushes file data, which is written in place ("ordered mode"), so metadata never points at unwritten blocks.
2. Writes a descriptor, the block images and a commit record with a CRC over all of them to the journal, then flushes.
3. Copies the images to their home locations.

On mount, a complete, checksummed transaction left in the journal is replayed, and a torn one is ignored. Freed blocks are not reused until the transaction that freed them commits. Freeing a block also revokes any journaled image of it, so a replay can never overwrite file data that later reused the block.

The same crate formats volumes on the host (`xtask/src/image.rs`). Its tests (`cargo test -p wavefs`) simulate power cuts by keeping only flushed writes, and check the result with `fsck`.

### FAT32 (`libs/fat32`)

The driver reads and writes FAT32 with VFAT long file names, generating `NAME~N.EXT` aliases. Since FAT has no inodes, the driver assigns stable inode numbers from each entry's location. Its tests cross-check against the independent `fatfs` crate in both directions.

## 6. Devices and power

### USB (`drivers/usb/`)

- **xHCI** (`xhci.rs`). After taking the controller from the firmware (USB legacy support) and resetting it, the driver works from rings of 16-byte TRBs: a command ring, a transfer ring per endpoint, and an event ring on which the controller reports completions and port changes (with an MSI). Device state lives in contexts; the driver fills in input contexts to address and configure devices.
- **Two tasks per controller.** `xhci-events` drains the event ring: it completes waiting transfers, feeds interrupt pipes (key presses, pointer motion, hub changes) to their drivers, and reports port changes. `usb` handles those changes: it resets ports, enumerates new devices (address, descriptors, configuration) and hands them to class drivers, or tears down unplugged ones. Transfers may be issued from any task; the controller lock is held only while TRBs are queued.
- **Class drivers.** `hub.rs` (USB 2 and 3 hubs, with routes and transaction translators), `hid.rs` (boot keyboards; mice and absolute tablets from a report-descriptor parser), `msc.rs` (SCSI over Bulk-Only Transport as a `BlockDevice`; FAT volumes on GPT, MBR or whole-disk media mount at `/Volumes/<label>` and are unmounted on unplug or eject).

### Sound (`drivers/audio/`)

- **Intel HDA** (`hda.rs`). The controller talks to codecs over the CORB/RIRB rings (with the immediate-command registers as fallback). In the audio function group the driver walks the widget graph from every output pin (speaker, headphones, line out) back to a DAC, selects, unmutes and powers that route, and binds every DAC to one output stream: a looping ring of eight 1024-frame buffers at 48 kHz, 16-bit stereo, interrupting (MSI) after each. Headphone jacks with presence detection mute the speakers.
- **Mixer** (`mod.rs`). Programs write samples to playback streams (`AUDIO_OPEN` gives a file descriptor with back-pressure). The `audio` task sums the streams, applies the master volume (a square curve) and a soft knee, and keeps the ring filled about 64 ms ahead of the device. System sounds are WAV files synthesized at build time into `/System/Sounds`.

### ACPI (`acpi/`)

- **Static tables** (`tables.rs`) are parsed early: MADT, FADT, MCFG, HPET, and `\_S5` by a pattern scan as a fallback.
- **The runtime** (`runtime.rs`) runs the `acpi` crate's AML interpreter on its own `acpi` task, with a kernel `Handler` (`handler.rs`: physical memory, ports, PCI configuration, time, AML mutexes). It loads the DSDT and SSDTs, initializes devices, reads the sleep states and finds batteries (`_BIX`/`_BIF`, `_BST`), the power adapter (`_PSR`) and the lid (`_LID`). It then waits for SCI events or a 30-second timer to refresh them.
- **Isolation.** All AML runs on that task. If the interpreter panics on unusual firmware, the panic handler ends only that task: ACPI features switch off and the system keeps running.
- **The SCI** (`events.rs`) acknowledges the power and sleep buttons and masks fired general-purpose events; the task runs their `\_GPE._Lxx`/`_Exx` methods and re-enables them.

### Sleep (`power/s3.rs`)

1. On CPU 0, after syncing the filesystems and running `\_PTS(3)`, the other CPUs are parked.
2. The IOAPIC routes and the PCI configuration of every device are saved, the FACS waking vector is pointed at the trampoline page, and the CPU context is saved like `setjmp`.
3. `SLP_TYP`/`SLP_EN` power everything down but RAM.
4. On wake, the firmware starts CPU 0 in real mode at the trampoline, which enters long mode and calls `resume_entry`. That reloads the kernel page tables, the GDT and TSS (clearing its busy bit), the IDT, `syscall` and FPU setup, and returns into `suspend` as if the save had just returned.
5. `suspend` then restores the clocks, the APICs, PCI configuration and MSI/MSI-X routes, each driver's hardware (`drivers::resume`: AHCI, NVMe, virtio, HDA, xHCI, display mode, PS/2), ACPI events and `\_WAK`, and finally restarts the other CPUs.

## 7. Networking (`drivers/net/`, `net/`)

### Adapters

Both drivers implement the `net::Nic` trait (`mac`, `link_up`, `send`, `poll`) and hand received frames to `net::receive`.

- **virtio-net** (`drivers/net/virtio.rs`) uses the shared modern virtio-pci transport (`drivers/virtio.rs`): receive queue 0 with MSI-X, transmit queue 1 whose buffers are reclaimed lazily, a 12-byte header per frame.
- **e1000/e1000e** (`drivers/net/e1000.rs`) runs 256-entry descriptor rings with 2 KiB buffers. The 82574L uses MSI-X (routed through `IVAR`); cards without MSI are polled by the network task.

### The stack

- **One task.** `net::init` starts a `net` kernel task that drains the receive queue, processes frames, and runs timers (ARP retries, TCP retransmissions, delayed ACKs, TIME_WAIT, DHCP) at least every 10 ms. The stack's state sits behind one mutex. After each round the task bumps a generation counter and wakes waiters, so a socket's wait never holds the stack's lock while sleeping.
- **Layers.** Ethernet and ARP (unresolved packets wait in a queue for up to 3.5 s), IPv4 without fragmentation, ICMP (echo, and unreachable reported to TCP), UDP, and TCP (`net/tcp.rs`): the full state machine, MSS 1460, window scale 3, 256 KiB buffers, out-of-order segments kept, RFC 6298 timers, slow start and congestion avoidance, fast retransmit on three duplicate ACKs, zero-window probes, a 5-second TIME_WAIT and RFC 1337.
- **Interfaces.** `lo` (127.0.0.1) plus one `eth<n>` per adapter. The DHCP client (`net/dhcp.rs`) gets the address, router and DNS servers, and renews at half the lease. The desktop hears about changes and redraws the menu bar.
- **Sockets** (`net/socket.rs`) are file descriptors: TCP (connect, listen, accept, shutdown), UDP datagrams, and ICMP "ping" sockets (the kernel fills in the identifier and checksum). Reads and writes block with a timeout, or return `EAGAIN` in non-blocking mode.
- **Randomness** (`random.rs`) is a ChaCha20 generator seeded from RDSEED/RDRAND and timing jitter, rekeyed after use; it gives TCP its initial sequence numbers and programs `GETRANDOM`.

## 8. The web (`libs/web`, `libs/tls`, `libs/surf`, `userland/apps/surf`)

All three libraries are `no_std` and make no system calls. Programs supply connections, text measurement and images, so the libraries run and are tested on the host.

- **HTTP** (`libs/web`). `http::fetch` follows redirects over any `Connect`or. `libaurora::web` provides one that resolves names (DNS over UDP), opens TCP or TLS connections, and keeps idle ones in a small per-process pool (15 s, up to 8), retrying on a fresh connection if an idle one was closed by the server.
- **TLS** (`libs/tls`). The client offers TLS 1.3 and 1.2. In 1.3 it sends an X25519 key share, answers a HelloRetryRequest with P-256, runs the HKDF key schedule, and checks `CertificateVerify` and `Finished`. In 1.2 it does ECDHE with AEAD ciphers, the extended master secret and the downgrade check. Certificates are parsed by our DER reader and validated to one of Mozilla's roots (packed by xtask into `/System/Certificates/roots.bin`): names (wildcards and IP addresses), validity dates, CA flags and signatures (RSA PKCS#1 and PSS, ECDSA P-256 and P-384). The primitives come from RustCrypto.
- **The engine** (`libs/surf`):
  - `html.rs` tokenizes and builds a tree the forgiving way: implied `html`/`head`/`body`, implied end tags, raw-text elements and character references.
  - `css.rs` parses stylesheets (with `@media` and `@supports`) and matches selectors right to left. `style.rs` runs the cascade: the user-agent sheet, presentational attributes, author sheets and `style` attributes, with `!important`, inheritance, custom properties, `calc()`/`min()`/`max()`, and `::before`/`::after` content. Rules are bucketed by id, class and tag, and each element carries a 256-bit filter of its ancestors' ids, classes and tags, so most descendant selectors are rejected without walking the tree.
  - `layout.rs` produces a display list of rectangles, text runs, images and bullets, plus link and form-field areas and `#fragment` anchors. It covers block flow with margin collapsing; inline formatting measured in 1/64 px, breaking only at break opportunities; lists; tables (automatic column widths, spans); flex rows; grid (tracks, areas, spans, auto-placement); floats as rows; and form controls.
- **Surf** (`userland/apps/surf`) loads a page and its stylesheets on a worker thread and pictures on three more, runs layout and paints on the main thread, and keeps the computed styles so pictures arriving later only redo layout.

## 9. Telemetry and the System Explorer (`telemetry.rs`, `xtask/src/monitor.rs`)

When QEMU attaches a second UART (COM2), `telemetry::init` detects it with a scratch-register test and the kernel starts streaming newline-delimited JSON:

- **Events:** `stage` (boot progress), `proc` (start, exit, crash), `win` (open, close), `mount`, `dev`.
- **Snapshots:** a `telemetry` task writes one every 250 ms. It carries every task with its CPU time and CPU, the scheduler's switch log since the last snapshot (with the CPU of each switch), per-CPU busy and idle time, device interrupt vectors, processes, memory, per-syscall and per-IRQ counters, per-device I/O counters, mounted filesystems, and the window list.

Lines are written while holding the port lock and without allocating, so events are safe before the heap exists. Without COM2 the hooks cost one relaxed atomic increment.

`cargo xtask run --monitor` works like this:
- QEMU connects COM2 to a Unix socket owned by xtask, and xtask turns the COM1 console into `console` events.
- xtask serves `docs/explorer/index.html` plus a Server-Sent Events stream at `/events`. The stream replays history, so a page opened late still sees the boot.
- The page is a single self-contained file with no dependencies. With no server it plays `docs/explorer/demo.js`, a recorded session.

## 10. Testing

- **`cargo xtask test`.** It builds the kernel with `--features ktest`. The tests in `kernel/src/tests.rs` cover:
  - the frame allocator and the heap
  - integer math and rectangle math
  - the keyboard decoder and RamFS
  - timer accuracy
  - preemptive scheduling and task reaping
  - wallpaper generation
  - address-space teardown, checked for frame leaks
  - the user-space `usertest` suite, which exercises every syscall: bad pointers, pipes, child processes, exit codes, and a crashing child
  - keyboard layouts, AltGr and dead keys
  - the TrueType engine with the installed faces
  - Spotlight's calculator and file index, the clipboard, and the Trash with Put Back
  - storage: a GPT read, a write-and-verify round trip on the real controller (checking that completions arrive by interrupt), WaveFS through the kernel adapter on a RAM disk, WaveFS on the home volume checked with `fsck`, and FAT32 on `/Boot`
  - SMP: work on every CPU with lock stress, TLB shootdowns, the TSC against the HPET
  - user copies that fault, recovering with `EFAULT`
  - sound: a tone through HDA (the host also checks that QEMU's WAV recording isn't silent)
  - ACPI: AML, sleep types, the test battery, adapter and lid; and that a failing AML task leaves the system running
  - USB: a hub with a keyboard behind it, a tablet, and a FAT32 stick mounted, read and written
  - sleep: S3 (the harness wakes the VM over QMP), then CPUs, work on every CPU, disk I/O, sound, USB and the network after waking
  - networking: DHCP, ping to loopback and our own address, UDP and TCP over loopback (a megabyte each way), a megabyte over HTTP from a server xtask runs on the host, and an inbound connection through a QEMU port forward

  `usertest` also covers threads, futexes and SIMD state surviving preemption.

  The kernel reports through QEMU's `isa-debug-exit` device. `xtask test` boots the same fresh disk **twice**: the first boot leaves a file, and the second must find it intact.
- **Headless boots.** `cargo xtask run --headless` exposes QEMU's HMP monitor at `target/qemu-monitor.sock` and QMP at `target/qemu-qmp.sock`. That makes it possible to script screenshots and input (`screendump`, `input-send-event`).
- **Host tests.** `cargo test -p wavefs -p fat32 -p aurora-gfx -p aurora-image -p aurora-wav -p aurora-web -p aurora-tls -p aurora-surf -p xtask` covers the filesystem libraries (crash recovery, `fatfs` cross-checks), the TrueType engine (metrics and placement against fontdue, coverage against 4× supersampling), the image codecs (against the `png`, `jpeg-encoder`, `jpeg-decoder` and `gif` crates), WAV decoding, URLs and HTTP (redirects, keep-alive, gzip), TLS (every cipher suite and version against a rustls server, RFC 8448 key schedule, bad certificates), the browser engine (parsing, selectors, the cascade and layout) and the AML assembler for the test SSDT.
- **Scripted UI.** `tools/qmp.py` clicks, types and takes screenshots in a running VM.
- **CI.** On every push, CI runs the host tests, the kernel suite on AHCI, virtio-blk and NVMe with 4 CPUs (and AHCI with 1 CPU), with virtio-net (and e1000e on one job), and a headless boot to the desktop.
