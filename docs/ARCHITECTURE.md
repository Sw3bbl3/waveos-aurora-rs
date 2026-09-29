# WaveOS Aurora architecture

This document is for someone about to read or change the code. It follows the machine from power-on to the desktop, then covers each subsystem.

## Repository layout

| Path | Crate | Target | Role |
|---|---|---|---|
| `bootloader/` | `aurora-boot` | `x86_64-unknown-uefi` | UEFI application: loads the kernel and the system image |
| `kernel/` | `tide` | `x86_64-unknown-none` | The kernel and the Crest window server |
| `libs/bootinfo/` | `bootinfo` | both | `#[repr(C)]` handoff contract |
| `libs/abi/` | `aurora-abi` | kernel + user | System call numbers, errors, `#[repr(C)]` structs, key types |
| `libs/gfx/` | `aurora-gfx` | kernel + user | Rasterizer, fonts, theme, icons, widgets, wallpapers |
| `libs/elf/` | `aurora-elf` | bootloader + kernel | Overflow-checked ELF64 reader |
| `libs/wavefs/` | `wavefs` | kernel + host | The WaveFS filesystem engine (also used by `mkfs` in xtask) |
| `libs/fat32/` | `fat32` | kernel | FAT32 with long file names |
| `userland/libaurora/` | `aurora` | user | Runtime: entry point, heap, `print!`, files, processes |
| `userland/ripple/` | `ripple` | user | UI toolkit: windows, event loop, the `App` trait |
| `userland/apps/*` | `app-*` | user | Files, Terminal, Notes, Calculator, Settings, About, Welcome |
| `userland/bin/*` | — | user | Command-line tools (`coreutils`), `usertest`, `crashtest` |
| `xtask/` | `xtask` | host | Build, run, image and test orchestration |
| `assets/fonts/` | — | — | TTFs rasterized at build time |
| `assets/home/` | — | — | Default folders and documents for a new home volume |
| `tools/qmp.py` | — | host | Scripted clicks, typing and screenshots for a running VM |

The root workspace's `default-members` is only `xtask`, so a plain `cargo build` never tries to build the kernel for your host. `userland/` is a separate workspace. `xtask` builds it with the user linker script (`userland/libaurora/user.ld`, base `0x40_0000`) and packs the binaries into `system.tar`:
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
serial → GDT/TSS → IDT → syscall MSRs → mm (frames, heap, drop identity map, vmm)
      → ACPI → APIC → PS/2 + IRQ routing → VFS (RamFS at /, TarFS at /System)
      → scheduler → reaper → sti → PCI → block drivers + GPT → mount WaveFS at /, FAT32 at /Boot
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

### Interrupts

- **Exceptions.** All exceptions have handlers. Double faults and page faults run on their own IST stacks, so a kernel stack overflow still produces a readable crash screen.
- **Legacy PIC.** It is remapped to vectors `0xE0`–`0xEF` and fully masked.
- **Local APIC timer.** It is calibrated against PIT channel 2 and runs periodically at **1 kHz** (vector 32).
- **I/O APIC.** It routes ISA IRQ1 (keyboard, vector 33) and IRQ12 (mouse, vector 44). It honours the MADT interrupt source overrides, which cover polarity and trigger mode.

### Scheduling (`sched/`)

- **Model.** Kernel threads, with a 128 KiB heap-allocated stack each. The boot context becomes task 0, the idle task.
- **Policy.** Round-robin with a **10 ms quantum**, preempted from the timer interrupt. Tasks can `sleep_ms`, `yield_now`, or `wait_until(timeout, condition)`. The last is how the compositor sleeps until input arrives: IRQ handlers call `sched::wake`.
- **Context switch.** In `arch/switch.rs`, it saves only callee-saved registers, because every switch happens inside a function call. The kernel is built soft-float, so there is no FPU state to save.
- **Locking rule.** On one core with preemption, a plain spinlock held across a preemption deadlocks. Shared state therefore uses `sync::IrqMutex`, which keeps interrupts off while it is held.

### Input

The IRQ handlers decode input straight into a fixed-size, allocation-free ring buffer (`drivers/input.rs`):

- **Keyboard.** Scancode set 1, US layout (`drivers/keyboard.rs`).
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
- **CPU faults in ring 3** kill only the faulting process (`proc::crash_current`), and the desktop shows a Problem Report.
- **`kill`** is cooperative. The target exits itself at a safe point (syscall return, a timer tick in ring 3, or a blocking wait), so it never dies while holding a kernel lock.
- **The `reaper` kernel task** frees exited processes: it closes handles (so pipe peers see EOF), closes windows, and drops the address space.

### libaurora and Ripple
- **libaurora:** the runtime. It provides `_start`, a heap that grows by `mmap`-ing new arenas, and `print!`, which formats into one buffer and writes it once. It also has file, process and time APIs.
- **Ripple:** the UI toolkit. An app implements `ripple::App` (draw/key/click/hover/scroll/tick) and calls `ripple::run`. Ripple creates the window, draws the app with `aurora-gfx` into the shared surface, calls `win_present`, and turns window-server events into method calls.

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
- **Built-in dialogs.** A few system dialogs (Power, Problem Report) are built into the server and implement the kernel-side `App` trait directly.
- **Rendering primitives.** Rendering is `aurora-gfx`:
  - integer anti-aliased shapes, SDF shadows and frosted glass
  - glass samples a wallpaper blurred once at startup
  - fonts are pre-rasterized at build time (Inter and JetBrains Mono)
  - wallpapers are procedural
- **Desktop** (`desktop/mod.rs`): the window stack, focus, drag and resize, zoom and minimize.
- **Shell** (`desktop/shell.rs`): the menu bar, dock, launcher and menus. The catalog in `apps/mod.rs` maps app names and icons to program paths.

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

## 6. Testing

- **`cargo xtask test`.** It builds the kernel with `--features ktest`. The tests in `kernel/src/tests.rs` cover:
  - the frame allocator and the heap
  - integer math and rectangle math
  - the keyboard decoder and RamFS
  - timer accuracy
  - preemptive scheduling and task reaping
  - wallpaper generation
  - address-space teardown, checked for frame leaks
  - the user-space `usertest` suite, which exercises every syscall: bad pointers, pipes, child processes, exit codes, and a crashing child
  - storage: a GPT read, a write-and-verify round trip on the real controller, WaveFS through the kernel adapter on a RAM disk, WaveFS on the home volume checked with `fsck`, and FAT32 on `/Boot`

  The kernel reports through QEMU's `isa-debug-exit` device. `xtask test` boots the same fresh disk **twice**: the first boot leaves a file, and the second must find it intact.
- **Headless boots.** `cargo xtask run --headless` exposes QEMU's HMP monitor at `target/qemu-monitor.sock` and QMP at `target/qemu-qmp.sock`. That makes it possible to script screenshots and input (`screendump`, `input-send-event`).
- **Host tests.** `cargo test -p wavefs -p fat32` covers the filesystem libraries, including crash-recovery and `fatfs` cross-checks.
- **Scripted UI.** `tools/qmp.py` clicks, types and takes screenshots in a running VM.
- **CI.** On every push, CI runs the host tests, the kernel suite on AHCI, virtio-blk and NVMe, and a headless boot to the desktop.
