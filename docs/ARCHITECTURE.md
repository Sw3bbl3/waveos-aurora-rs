# WaveOS Aurora architecture

For the versioned 0.7 user and developer guides, see the [bilingual handbook](handbook/README.md). This reference URL remains available.

This document is for someone about to read or change the code. It follows the machine from power-on to the desktop, then covers each subsystem.

## Repository layout

| Path | Crate | Target | Role |
|---|---|---|---|
| `bootloader/` | `firstlight` | `x86_64-unknown-uefi` | UEFI application: loads the kernel and the system image |
| `kernel/` | `aster` | `x86_64-unknown-none` | The kernel and the Lumen Server window server |
| `libs/bootinfo/` | `bootinfo` | both | `#[repr(C)]` handoff contract |
| `libs/abi/` | `aurora-abi` | kernel + user | System call numbers, errors, `#[repr(C)]` structs, key types |
| `libs/gfx/` | `lumen` | kernel + user | Rasterizer, our TrueType engine, theme, icons, widgets, wallpapers |
| `libs/edid/` | `aurora-edid` | kernel + host | EDID parsing (base block, CTA-861, DisplayID) and CVT timings |
| `libs/amdgfx/` | `aurora-amdgfx` | kernel + host | AMD display engine registers (DCE 11.2/12, DCN 1.0–3.2, generated from AMD's headers by `tools/amd-regs.py`): device table, firmware readback and the takeover fingerprint, flips, cursor, DP AUX engine, HPD, VRAM placement, and a simulated engine for tests |
| `libs/nvgfx/` | `aurora-nvgfx` | kernel + host | Nvidia GPU identification (device ids, `NV_PMC_BOOT_0`) |
| `libs/igfx/` | `aurora-igfx` | kernel + host | Intel display engine programming (display 9–13): device table, firmware readback, planes, DDB/watermarks, GGTT, GMBUS, DP AUX and link training, Skylake DPLLs, backlight, mode set sequences, and a simulated engine for tests |
| `libs/image/` | `aurora-image` | kernel + user + host | PNG decoder and encoder; JPEG, GIF and BMP decoders |
| `libs/elf/` | `aurora-elf` | bootloader + kernel | Overflow-checked ELF64 reader |
| `libs/aurorafs/` | `aurorafs` | kernel + host | The AuroraFS filesystem engine (also used by `mkfs` in xtask) |
| `libs/fat32/` | `fat32` | kernel | FAT32 with long file names |
| `libs/wav/` | `aurora-wav` | kernel + user + host | WAV decoding (any rate → 48 kHz stereo) and encoding |
| `libs/web/` | `nebula-web` | user + host | URLs, and an HTTP/1.1 client over any byte stream (keep-alive, chunked, gzip) |
| `libs/tls/` | `nebula-secure` | user + host | TLS 1.3/1.2 client, DER and X.509 path validation |
| `libs/surf/` | `nebula-engine` | user + host | The browser engine: HTML, CSS, style and layout to a display list |
| `libs/hid/` | `aurora-hid` | kernel + host | HID report descriptors; keyboards, pointers, touchpads (and their gestures), touch screens, game controllers |
| `libs/acpires/` | `aurora-acpi-res` | kernel + host + xtask | ACPI resource descriptors (`_CRS`: memory, interrupts, `GpioInt`, I²C/SPI/UART serial bus), `ToUUID`, EISA ids |
| `libs/i2c/` | `aurora-i2c` | kernel + host | The I²C bus trait, the DesignWare I²C controller, the HID over I²C protocol, and a software HID-over-I²C device |
| `userland/corekit/` | `aurora` | user | Runtime: entry point, heap, `print!`, files, processes, threads, clipboard, drag and drop, preferences |
| `userland/aurorakit/` | `aurorakit` | user | UI toolkit: windows, event loop, the `App` trait, the `text` editing engine |
| `userland/apps/*` | `app-*` | user | Files, Terminal, Notes, Calculator, Settings, Lens, Paint, Clock, Pulse, About, Welcome, Learn, Studio, Installer |
| `userland/bin/*` | — | user | Command-line tools (`coreutils`), `pkg` (apps), `usertest`, `crashtest`, `pkgtest` |
| `libs/gina/` | `gina` | kernel + user + host | GINA app packages: manifest, archive checks, hash lists, Ed25519 signatures (feature `crypto`, off in the kernel), repository indexes, version order |
| `xtask/` | `xtask` | host | Build, run, image and test orchestration; system sounds, Mozilla's root CAs (from `webpki-roots`), a test SSDT (battery, lid) and a USB-stick image |
| `assets/fonts/` | — | — | Manrope and JetBrains Mono, shipped in `/System/Fonts` |
| `assets/home/` | — | — | Default folders, documents and pictures for a new home volume |
| `assets/icons/`, `assets/wallpapers/`, `assets/wallpaper-thumbs/`, `assets/brand/` | — | — | App icons, the Tide, Aurora and Dunes wallpapers (day and night) with their Settings pictures, and the WayV logos; shipped in `/System/Icons`, `/System/Wallpapers` and `/System/Brand` |
| `tools/qmp.py` | — | host | Scripted clicks, drags, typing and screenshots for a running VM |
| `tools/make_sample_pictures.py` | — | host | Generates the sample pictures (procedural) |

The root workspace's `default-members` is only `xtask`, so a plain `cargo build` never tries to build the kernel for your host. `userland/` is a separate workspace. `xtask` builds it for our own target, `userland/x86_64-aurora-user.json` (ring 3 with SSE2; the kernel stays soft-float), rebuilding `core` and `alloc` with `-Zbuild-std`, links it with `userland/corekit/user.ld` (base `0x40_0000`), and packs the binaries and fonts into `system.tar`:
- `app-*` binaries become `/System/Apps/<Name>.elf`
- everything else becomes `/System/Bin/<name>`

## 1. Boot: `firstlight`

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
      → PCI → display (Bochs VGA or the firmware framebuffer) → block drivers + GPT (MSI/MSI-X)
      → mount AuroraFS at /, FAT32 at /Boot → GPU drivers (Intel display, `intel=`; AMD display, `amd=`; Nvidia detection; virtio-gpu, `gpu=`)
      → USB (xHCI) → sound (HDA, device registry + mixer) → ACPI runtime (AML task)
      → flusher → spawn "lumen-server" (restores settings, launches Welcome) → idle
```

### Memory layout

| Virtual range | Contents |
|---|---|
| `0x0000_0000_0040_0000 …` | user program (per process) |
| `0x0000_0100_0000_0000 …` | user `mmap` region: heap arenas, window surfaces (base randomized, up to +1 TiB) |
| `…0x0000_7fff_f000_0000` | user stack (1 MiB, guard page below; top randomized, up to −4 GiB) |
| `0xffff_8000_0000_0000 + phys` | physical memory window (`mm::phys_to_virt`) |
| `0xffff_c000_0000_0000 …` | kernel mappings of window surfaces and large heap blocks (`vmm::kmap`) |
| `0xffff_d000_0000_0000 …` | kernel task stacks, 128 KiB each in a 256 KiB slot whose lower half is an unmapped guard (`mm::kstack`) |
| `0xffff_ffff_8000_0000 …` | kernel image (`kernel/linker.ld`) |

- **Frames** (`mm/frame.rs`): a buddy allocator over the usable regions above 1 MiB (including RAM above 4 GiB), with free blocks of 4 KiB to 4 MiB on lists linked through the free frames, in two zones: below 4 GiB (`alloc_contiguous_below`, for 32-bit devices) and above. Normal allocations take low memory first. Single frames go through a 64-frame cache per CPU, refilled and drained 32 at a time; runs come from the lists without scanning (runs over 4 MiB fall back to a scan). Two bitmaps (in use; free block starts here) catch double frees. When memory runs out, the caches are drained and the heap gives back its idle slabs before an allocation fails.
- **What programs may take.** A kernel reserve (1/16 of RAM, 32–128 MiB) is out of their reach: their pages, page tables, window surfaces, shared memory and mapped-file pages come from `frame::alloc_user`, which refuses once free memory is down to it, and kernel memory they cause is checked against it too (a thread's or process's kernel stack: `EAGAIN`/`ENOMEM`; a pipe or TCP buffer that would grow: the writer waits, or `ENOMEM`). Per-process limits bound the rest: 64 threads (512 for Linux programs), 1024 descriptors, 8 windows, 65,530 lazy mappings, 1,024 queued messages per local socket. Buffers sized by a system call's arguments are allocated fallibly (`syscall::user::buffer`). Page tables that no longer map anything are freed on `munmap`. When memory runs short (a program reached the reserve, or a kernel allocation failed), the reaper task has every cache give back what it can within a second (`mm::reclaim`: cached kernel stacks, block caches, mapped-file pages, idle heap slabs, per-CPU frames), and block caches keep an eighth as much meanwhile. `mm::report` says where memory is; `AURORA_MEM_REPORT=5` prints it every 5 s during the fuzzers, and it is printed when a kernel heap allocation fails.
- **Heap** (`mm/heap.rs`): grows on demand from the frame allocator, used through the physical window, so it needs no mappings and anything under a megabyte is physically contiguous. Up to 32 KiB, 40 size classes (four per power of two) come from slabs of 64–512 KiB; each CPU keeps a small stack of free objects per class, so most allocations and frees are a push or pop under that CPU's own lock with interrupts off for a few instructions, and batches move to and from the slabs under a per-class lock. Empty slabs go back to the frame allocator. Bigger allocations are runs of frames, and a megabyte or more is mapped into the kernel virtual area. The `heap-debug` feature adds red zones and poisoning. Statistics (per-class use, cache hit rates, free frames, the longest interrupts-off stretch) go out with the telemetry snapshot.
- **Paging** (`mm/paging.rs`): removes the identity map. It also provides `map_mmio` for devices beyond the physical window.
- **When memory runs short** (`mm/kswapd.rs`, `mm/swap.rs`, `mm/swapfile.rs`, `libs/lz4`). Three watermarks: *min* is the kernel reserve; under *low* (min + 1/64 of RAM) the `kswapd` task wakes and works until free memory is over *high* (min + 1/32). In order: the kernel's caches give back what they hold (block caches, the mapped-file cache, cached kernel stacks, idle heap slabs); cached shared pages nobody maps any more are written out; then it goes round the address spaces, background programs and bigger ones first, the program in front last, swapping out pages not used since it last passed (a clock over the page tables: the accessed bit gives a page a second chance; dirty bits are harvested to retry pages that didn't compress once they change). An unpressured system pays nothing: `kswapd` sleeps, and allocations only compare free memory with the watermark every 32 user frames, as they already read it.
  - *Where pages go.* A swapped-out page's entry holds a swap *slot* instead of a frame, with its protection (`vmm::SWAPPED`). A page of one repeated 64-bit word is kept as that word; others are compressed with LZ4 if they shrink to 3/4, into a pool of whole frames carved into 64-byte size classes (up to a quarter of memory and twice as many pages as memory holds — the zram of WaveOS); the slot table (40 bytes a page, at most a quarter of the reserve) and the pool are single frames linked through the physical window, so swap never needs the heap to grow when memory is short and broken up; the rest go to `/Security/swapfile`, made on the system volume the first time memory runs short (half of RAM, 4 MiB–2 GiB, at most an eighth of the free space; none on a read-only, full or in-memory volume). The filesystem allocates its blocks without writing them and refuses every access to it from then on; pages go straight to its sectors (never through the block cache), each encrypted with ChaCha20 under a key made at boot and kept only in memory, with a nonce per write — so swap can't leak between processes (a freed slot's data is never read again, freed compressed pages are wiped) or across boots. A failing disk leaves pages in memory; after four write errors the file takes no more.
  - *Shared pages without a reverse map.* Frames more than one holder may map are *counted* (shared memory, pages shared since `fork`, mapped files): holders are page table entries, objects (an open memfd, the mapped-file cache, the vDSO) and the slot caching the frame. Each address space's entry is swapped out on its own as the scan reaches it — it takes the frame's slot (made the first time, caching the frame) and lets go of the frame — and a fault maps the cached frame again, so processes keep sharing the page. Only when the slot is the last holder is the frame written out and freed. Frames an object holds stay in memory, and so do frames a shared futex waits on (pinned: the futex key is the frame's address). Locked pages (`mlock`) are never swapped out, and neither are GPU buffers mapped into programs (dumb buffers, dma-bufs: `vmm::shm_count` counts them as device frames with a pin that never comes off — a display may scan them out, another device read them — and the last holder frees them).
  - *SMP.* The scan takes an address space's lock with `try_lock` only, clears accessed bits atomically, swaps an entry for its slot with a compare-and-swap (a page the CPU just used or wrote stays), and reads a page only after a TLB shootdown of every CPU running that address space.
  - *Waiting, and the deadlock rule.* Only `kswapd` evicts; allocations never do. The rules: **nobody waits for memory while holding an address-space lock** (page faults, `mmap` and system calls that find memory at the reserve let go of it and wait outside, in `proc::with_memory`; allocations under the lock use `frame::alloc_user_nowait`), **`kswapd` never waits for an address-space lock** (`try_lock`; a busy one is skipped), and a wait is only with interrupts on (so no spinning lock is held) and for at most a second. So whatever a waiter holds, either `kswapd` gets on or the waiter gives up: `mmap` (native: memory is made at once, a chunk at a time, with the address space let go in between so its own earlier pages can be swapped) and system calls fail with `ENOMEM`; a page fault goes to the out-of-memory killer. Kernel allocations never wait.
  - *Bringing pages back* (a fault on a swapped-out page) takes a frame without waiting, and may dip into the top eighth of the reserve: the page's slot is freed, so `kswapd` can swap an idle page out in its place. So when memory and swap are both full, a program touching its own memory goes on as long as something idle can make room. Pages in the swap file that follow the faulting one, in the address space and on the disk, are read along with it (up to 8, one request) when memory isn't short; pages going to the swap file together are written in one request.
  - *Out of memory.* When reclaim can't keep up (a fault finds no memory after ten seconds of waiting, or `kswapd`, in a round begun since the fault started waiting, finds swap full or nothing left to swap out; the log says which), one program is ended by a score: the pages ending it frees (in memory and swapped out), an eighth of that for the program in front, half for programs with windows, nothing for processes marked `oom_protected`, which go only if nothing else is left; ties go to the newest. One at a time (others wait for the victim to go), with a notification ("Out of memory", naming it) and a log line with the memory report. The kernel itself never panics for want of memory for programs: its reserve stays.
  - *Pressure.* Time some program spends waiting for memory is accounted (10/60/300 s averages, Linux `/proc/pressure/memory` "some"); the level (none, warning, critical) goes to every window as an `event::MEMORY_PRESSURE` when it changes, and with the memory, compressed and swap figures to `sys_stats` (the system monitor) and the telemetry stream (the System Explorer). Linux programs get `/proc/meminfo` (swap, `Zswap`), `/proc/pressure/memory`, `/proc/<pid>/status` (`VmRSS`, `VmSwap`, `VmLck`), `sysinfo` swap, `madvise` `MADV_DONTNEED`/`FREE`/`PAGEOUT`/`COLD`/`WILLNEED`/`POPULATE_*`, and `mlock`/`munlock`/`mlockall`/`munlockall` (`MCL_FUTURE` is accepted, but later mappings aren't locked).

### Clocks

- **HPET** (`drivers/hpet.rs`), found through the ACPI `HPET` table, is the reference clock.
- **TSC.** When it runs at a constant rate (invariant TSC, or under a hypervisor), it is calibrated against the HPET (or PIT channel 2) and becomes the clock: `time::now_ns()` is a `rdtsc` and a multiply. Otherwise the HPET counter is the clock. An offset keeps time monotonic across sleep, when the counters may restart.
- **Real time** (`time::realtime_ns`, UTC) is the monotonic clock plus one offset, read from the RTC (which keeps local time in the `utc_offset` zone) at boot, and changed only when the clock is set, the zone is learnt from the settings, or the RTC disagrees by more than a second after sleep. The Linux vDSO's time page carries the same TSC base, rate and offsets, republished at each change, so its clocks agree with the system calls' by construction.
- **Local APIC timers** are calibrated against the HPET. There is **no periodic tick**: each CPU's timer is armed for that CPU's next deadline (see [Timers and tickless idle](#timers-and-tickless-idle)), in TSC-deadline mode where the CPU has it, else one-shot. The 1 kHz tick remains only when the tick is the clock (no TSC or HPET) or with `timer=periodic` in `boot.conf`.

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
- **Policy.** Round-robin with a **10 ms quantum**. A CPU's timer ends a slice only while another task waits for that CPU; a task alone on its CPU runs undisturbed. New tasks go to the least-loaded CPU. A woken task goes to its last CPU if that is idle; else to the waking CPU if it is idle (an interrupt handler on an idle CPU: it is awake already); else to another idle CPU, preferring one polling for work (no IPI needed); else it waits on its last CPU, whose slice timer is armed then (by IPI if it is another CPU). A task that gives way at the end of its slice moves to an idle CPU if there is one; a CPU that becomes idle steals from the busiest queue of a busy CPU (an idle CPU runs its own queue once its IPI wakes it).
- **Switching** is split: under the scheduler lock a CPU picks the next task and records the switch; then it swaps stacks. A task leaving a CPU keeps its `on_cpu` flag until that CPU has switched away (`finish_switch`), and another CPU only reads its saved stack pointer after taking the flag. Dead tasks are freed after the switch.
- **Waiting.** `wait_until(timeout, condition)` marks the task sleeping *before* testing the condition, so a wakeup from another CPU is never lost. `sync::WaitQueue` builds on it for devices: a driver's interrupt handler wakes the task waiting for its completion.
- **Pinning and parking.** `spawn_on_bsp` keeps a task on CPU 0 (sleep must start there). `park_aps` moves all work to CPU 0 and halts the others before sleep.
- **Idle time** is accounted from switch times (`cpu_times`), so it stays right with the tick stopped.
- **Locking rule.** A spinlock held across a preemption deadlocks, so shared state uses `sync::IrqMutex`, which keeps interrupts off while held. Beware `while let Some(x) = q.lock().pop()`: the guard lives for the whole loop body. Longer critical sections that may block use `sync::Mutex`, whose waiters yield at first and, once it has been held for 2 ms, sleep a millisecond between tries: waiters that only ever yield are always ready, and starved a holder that runs only when nothing else is (the boot code, in a CPU's idle task, holding a disk's queue lock — the second boot on one CPU with a virtio disk hung that way). In self-test builds, 60 s without any output while tasks keep switching prints every task's stack.

### Timers and tickless idle (`timer.rs`, `arch/timer.rs`)

- **Timer queues.** Every CPU keeps its pending timers ordered by deadline (ns of `time::now_ns`). A timer is armed on the CPU that asks for it, so arming never touches another CPU's hardware; it can be cancelled from any CPU. A timer either wakes a sleeping task or calls a short function in interrupt context (`timer::call_after_ms`). Every `sleep_ms` and `wait_until` timeout is one: sleeps end on millisecond boundaries as before (`time::ticks()` is unchanged), so timers due in the same millisecond share one interrupt.
- **The hardware.** Each CPU's LAPIC timer is armed for the earliest of its timers and, while other tasks wait for it, the end of the running slice (`sched::tick_deadline`). With TSC-deadline mode (CPUID.01H:ECX[24], and the TSC is the clock) the deadline is written to `IA32_TSC_DEADLINE`; otherwise the distance is counted down once (one-shot, divide-by-16; distances beyond 32 bits fire early and re-arm). QEMU's TCG has no TSC-deadline timer, so VMs use one-shot.
- **Idle.** An idle CPU arms nothing but its next timer and sleeps in an idle state (`power::idle`) until it or an interrupt or IPI comes. Wakeups are counted per CPU (`timer::STATS`: interrupts, expirations, slice ends, empty interrupts; IPIs received and avoided) and per task (timeouts that ran out), shown by `cpuinfo` and the telemetry.
- **Deep C-states** may stop the LAPIC timer on CPUs without ARAT (CPUID.06H:EAX[2]); there is no HPET broadcast, so states deeper than C1 are used only with ARAT (and a TSC that keeps running).
- **Periodic work** follows deadlines too: the network task sleeps until a frame, a socket or the stack's next TCP timer (once a second for link changes when quiet; every 10 ms only for cards without interrupts), the compositor samples the clock when its text can change (each minute, or each second with seconds shown), the Desktop folder is watched through the VFS's change hooks (`fs::index::wait_change`), USB, the reaper and LinuxKPI's worker sleep until kicked, and the sound mixer stops the device when nothing plays. An idle desktop in QEMU (4 CPUs, TCG) takes about 2 timer interrupts per CPU per second (it took 700–1000 with the tick).

### Input

The IRQ handlers decode input straight into a fixed-size, allocation-free ring buffer (`drivers/input.rs`):

- **Keyboard.** Scancode set 1 is translated to USB HID usages (`drivers/keyboard.rs`), and the chosen layout turns usages into characters (`drivers/keymap.rs`: U.S., British, German, French, Spanish, Swedish, with AltGr and dead keys). USB keyboards report usages directly and share the same tables; their key repeat is done in software.
- **USB pointers.** Mice, tablets and VM pointers are read through their HID report descriptors (see below).
- **Mouse, emulated.** Under QEMU and VMware, the **vmmouse** backdoor gives an absolute pointer, so the guest cursor tracks the host cursor without grabbing it.
- **Mouse, real hardware.** Standard PS/2 relative packets, with IntelliMouse wheel support.
- **Laptop touchpads and touch screens over I²C** (`drivers/i2c/`). The ACPI task finds HID over I²C devices (`PNP0C50`) and their controllers (`acpi/devices.rs`, see ACPI below); the `i2c` task drives the controllers (DesignWare I²C: Intel LPSS PCI functions, or ACPI-described Intel and AMD ones; interrupt-driven, polled without an interrupt), and each device's `i2c-hid` task speaks HID over I²C. Reports go through the same `aurora_hid` decoding and gesture engine as USB touchpads (`input::touchpad_output`). The device's interrupt is used when it is an I/O APIC input; on an AMD GPIO pin its level is sampled; otherwise (Intel GPIO) the device is polled, at 125 Hz while touched and 20 Hz idle.
- **Linux's view of the same devices** (`drivers/evdev/`, see [developer/linux-input.md](developer/linux-input.md)). Beside the queue above, each driver feeds its device as Linux describes it: event codes, capability bitmaps, state, and readers with Linux's framing, overflow and grab rules. Keyboards, pointers and touchpads come from the PS/2, USB HID and I²C HID drivers (`evdev::sources`); controllers from `drivers::gamepad` (`evdev::pad`, presenting each as `xpad`, `hid-playstation` or `hid-nintendo` would); raw HID reports from the USB HID driver (`evdev::hidraw`). Who reads follows the focus: the compositor names the focused window's process (`evdev::set_focus`, with the program behind a bridge's window from `win_set_program`) and Wave Play's controller owner (`gamepad::set_owners`). The Linux layer (`compat/linux/input/`) makes the files.

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

**Threads.** A process can run several threads (`thread_spawn`/`thread_exit`): tasks that share its address space and handles. `futex_wait`/`futex_wake` put contended threads to sleep, keyed by (address space, address); corekit builds `sync::Mutex`, `Condvar` and `thread::spawn`/`join` on them. `thread_exit` can store 1 into a word and wake it, which is how `join` knows the thread has left its stack.

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
- **CPU faults in ring 3** end only the faulting process (`proc::crash_current`), and the desktop shows a Problem Report — unless it is a Linux program with a handler for the signal the fault becomes (`SIGSEGV`, `SIGILL`, `SIGFPE`, `SIGBUS`, `SIGTRAP`): then the handler runs. Those exceptions, the timer and the reschedule IPI enter through `arch::trap`, which saves every register in a `TrapFrame` the handler can change, so a signal is also delivered to a thread interrupted in user code (a sender raises a reschedule IPI on the CPU running it).
- **The `reaper` kernel task** frees a process once the scheduler has retired its last thread: it closes handles (so pipe peers see EOF), closes windows, and drops the address space.

### Accounts and permissions (`users/`, `fs/perm.rs`)

- **Credentials.** Every process carries a `users::Cred`: uid, primary group, up to 16 supplementary groups, the *administrator capability* and a umask. Spawned and forked processes inherit their parent's (stdio handles change nothing); the desktop starts apps with the logged-in session's. Kernel tasks have none and act as the system. Ids: `0` system, `80` admin group, `100` users, accounts from `1000` (each with a private group of the same number). `cred_get`/`cred_set` read and narrow them (only the system may become another account); Linux programs see the same ids through `getuid`/`getgroups`/`stat`/`SO_PEERCRED`, and their `setuid` follows the same rule.
- **File permissions.** `fs::perm` checks Unix mode bits for the calling process at every VFS entry point: search on each folder walked, read/write on open, write+search on the folder for create, unlink and rename (with the sticky bit), read to list, execute for Linux programs (WaveOS programs need read). `/System` belongs to the system and is read-only; `/Boot` is the system's and the admin group's; removable volumes belong to the session. `/Security`, `/Users` and `/Applications` on the home volume can be created, removed or renamed only by the system. Kernel code keeping the system's own files during a call (preferences, the account database) runs under `fs::perm::system`.
- **Accounts** (`users::db`): `/Security/accounts.db` (`rw------- system` in `rwx------ system`, refused if not so), one section per account with a PBKDF2-HMAC-SHA256 hash, a 16-byte salt and the iteration count; failed checks wait longer after three. The first account's home is the volume root (the layout earlier versions made); others get `/Users/<name>`. `users` (list, add, remove, password, administrator, details) manages them.
- **The administrator capability** is held by administrators' processes, and given to others for five minutes when an administrator approves the system prompt (`elevate`, `apps/elevate.rs`; the window server reads the password, the app never does). Privileged: managing accounts, changing other accounts, `chown`, the clock, who logs in automatically, writing to `/Applications`. Signals and `kill` reach only the caller's own account's processes without it.
- **Shore** shows the first-run setup (password optional), the login screen with a row of accounts when needed, and the lock screen; the WayV menu logs out or switches accounts.
- Per-app sandboxing is designed in [developer/sandboxing.md](developer/sandboxing.md).

### corekit and AuroraKit
- **corekit:** the runtime. It provides `_start`, a heap that grows by `mmap`-ing new arenas, and `print!`, which formats into one buffer and writes it once. It also has file, process and time APIs.
- **AuroraKit:** the UI toolkit. An app implements `aurorakit::App` (draw/key/click/drag/hover/scroll/tick, drop targets, close requests) and calls `aurorakit::run`. AuroraKit creates the window, draws the app with `lumen` into the shared surface, calls `win_present`, and turns window-server events into method calls.
- **`aurorakit::text`:** one editing engine for every text box: `TextEdit` (caret, selection, word and line motion, undo grouped by typing bursts, the clipboard), a word-wrapping `TextView` with mouse selection (double-click a word, triple-click a line, drag with auto-scroll), and a single-line `TextField`.

## 4. Lumen Server window server (`kernel/src/gui/`)

- **Main loop.** Lumen Server is the `lumen-server` kernel task (`gui/mod.rs`, the frame in `gui/screens.rs`). Each frame it:
  1. drains input
  2. applies client commands (`server::take_commands`: window created, closed, presented, retitled; open app/file; theme; power) and crash reports
  3. repaints only the **damaged rectangles**, on each display the part that falls on it, into that display's back buffer
  4. presents them on the display (`drivers/gpu`): the back buffer is the framebuffer the display scans out, so on virtio-gpu only the damaged rectangles are transferred, and on VGA-style devices they are copied to video memory
- **Several displays** (`gui/screens.rs`, `desktop/displays.rs`, `lumen::displays`): every connected display of every device, arranged in one space of points with the main display (bar, Harbor, launcher, notifications, login, Wave Play) at the origin and the others touching it. Each has its own mode, scale, back buffer, wallpaper and damage, and with several, its own presenting task, so a slow display only skips its own frames. Windows move freely between displays, maximise and moor on the display they are on, open on the display with the pointer, leave an unplugged display for the main one and come back with it; the pointer crosses where displays touch. Mirroring copies the main display's picture. The arrangement, main display, mirroring and each display's resolution, scale and wallpaper are preferences keyed by the display's EDID identity. See [developer/display-core.md](developer/display-core.md#several-displays).
- **Client windows** (`server.rs`, `apps/client.rs`):
  - **Surface:** each window owned by a process has a surface, which is frames mapped both into the kernel (`vmm::kmap`) and into the process (`map_shared`).
  - **Compositing:** `ClientApp` adapts it to the window manager. It blits the surface with rounded bottom corners, and turns clicks, keys, scrolls, focus and resizes into `aurora_abi::Event`s on the window's queue.
  - **Events:** the process receives events through the blocking `next_event` syscall.
- **Built-in dialogs.** A few system dialogs (Power, Problem Report, "Keep this resolution?") are built into the server and implement the kernel-side `App` trait directly.
- **Rendering primitives.** Rendering is `lumen`:
  - integer anti-aliased shapes, SDF shadows and frosted glass
  - glass samples a wallpaper blurred once at startup
  - bilinear image scaling with correct transparent edges (`draw_image`)
  - wallpapers are procedural, or any picture
- **Text** (`libs/gfx/src/ttf`, `font.rs`) is our own TrueType engine. It reads `cmap`, `glyf` outlines (simple and composite), `hmtx`, OS/2 metrics and pair kerning (`kern` and GPOS), and rasterizes by exact signed-area accumulation in 16.16 fixed point, so the soft-float kernel can use it too. Glyphs are cached per face, size and quarter-pixel offset. Lumen Server and every app install the faces from `/System/Fonts`; a few build-time sizes cover the time before that (and the panic screen).
- **Desktop** (`desktop/mod.rs`): the window stack, focus, drag and resize, zoom and minimize. A press in a window's content captures the pointer until release, so apps get drags even outside their window.
- **Animations** (`desktop/anim.rs`): opening, closing, minimizing into the dock, restoring and zooming draw a snapshot of the window scaled and faded between two rectangles while the real window stays hidden. Client windows appear once they've drawn their first frame. Lumen Server runs at 60 Hz only while something moves.
- **Shell** (`desktop/shell.rs`): the menu bar, dock (with the Trash), launcher and menus. The catalog in `apps/mod.rs` maps app names and icons to program paths.
- **Wave Play** (`desktop/play/`): the controller layout. It never moves the desktop's windows; it covers them. Entering renders the desktop once into layers (wallpaper and items, each window, the bar's bubbles, Harbor's bays) and one spring-driven timeline (`lumen::spring`) recedes, slides and cross-fades them while the home's tiles rise in; reversing mid-way continues from the current position and velocity. The backdrop is a sixth-size buffer blurred once per focused tile and filtered up with `lumen::layer` (separable, SSE2 row blends in the compositor); the quick overlay blurs one still of the screen. Programs started from Wave Play are shown full screen (their window's content rect becomes the screen; the old rect is restored on leaving). Controllers reach it through `drivers/gamepad.rs`: connections and meaningful changes (a button, or a stick crossing into a direction) wake the compositor, presses are latched between frames, and while the shell has the controllers, programs read a neutral pad. `library.rs` gathers entries from the catalog, installed apps, `/Settings/Play/Library/*.play` files, Linux `.desktop` games and Steam's library manifests.
- **Installed apps** (`installed.rs`): apps installed from GINA packages, read from `/Applications` and the session account's `~/Library/Applications` (each app's atomic `active` pointer, then its manifest). Shoal, Seek and the dock show them with their own icons (`lumen::icons::custom`, a PNG checked to be at most 1024×1024 before it is decoded), games also under Play; the dock keeps the ones in the `dock_apps` preference and shows running ones (matched by program path). `REFRESH_APPS` drops the cache, so installs and removals show at once. Installing, the registry and repositories are user space: `corekit::apps` (see the GINA section of [WAVESDK.md](WAVESDK.md)).
- **Clipboard** (`clipboard.rs`): one item, text or a list of files; files paste as their paths where text is wanted.
- **Drag and drop** (`dnd.rs`, `desktop/dragdrop.rs`): an app calls `drag_start` during a press; Lumen Server takes the pointer, draws the drag image, sends `DRAG_OVER`/`DRAG_LEAVE` to windows underneath, and on release delivers `DROP` (the target fetches the payload with `drag_data`) and `DRAG_END` to the source. Dock apps open dropped files, and the dock Trash takes them.
- **Notifications** (`notify.rs`, `desktop/notifications.rs`): `notify` from apps, or the system; banners slide in at the top right, and the Notification Center (click the clock) keeps the history next to a calendar and Do Not Disturb.
- **Spotlight** (`desktop/spotlight.rs`, `fs/index.rs`): apps, files, settings panes, calculations and commands. The file index is built by a background task at boot and kept current by hooks in the VFS (create, mkdir, rename, unlink).
- **Preferences** (`settings.rs`, `prefs.rs`): `key=value` lines in `/Settings/aurora.conf`, read and written with `pref_get`/`pref_set` and applied at once: dark mode, the accent colour, keyboard layout and key repeat, clock format, Do Not Disturb, volume.
- **The display core** (`drivers/gpu/`, see [developer/display-core.md](developer/display-core.md)): display drivers expose Linux KMS objects (CRTCs, primary and cursor planes, encoders, connectors with EDID, refcounted buffer objects and framebuffers) and take atomic commits. Drivers: the firmware framebuffer (GOP), Bochs/QEMU std VGA, a native virtio-gpu driver (zero-copy scanout of the compositor's buffer, damage-only transfers, hardware cursor, multiple scanouts, host window resizes as hotplug), the ported Linux virtio-gpu driver (`gpu=linux`), an Intel display driver (`drivers/gpu/intel` over `libs/igfx`, see [developer/intel-display.md](developer/intel-display.md): takes over the firmware's pipes, scans out of RAM through the GGTT with a cache flush per damaged row, carves the cursor plane's display buffer share, GMBUS/AUX EDID, polled hotplug, PWM backlight via `gpu::set_backlight`; Skylake-class mode sets behind `intel=modeset`), an AMD display driver (`drivers/gpu/amd` over `libs/amdgfx`, see [developer/amd-display.md](developer/amd-display.md): takes over the firmware's timing generators only when what they scan matches the bootloader's framebuffer, flips between two buffers in VRAM above it, hardware cursor, DP AUX EDID, polled HPD; no mode setting), and Nvidia detection only (`drivers/gpu/nvidia.rs`, see [developer/nvidia-display.md](developer/nvidia-display.md)). Lumen Server is a client of it through `gpu::output::Output` (one per display it uses), and puts the pointer on the cursor plane of the display it is on. Linux programs reach it through the DRM device (`compat/linux/drm/`, `/dev/dri/card0`); `gpu::owner` decides who drives a device: a DRM master's first commit sets the desktop's state aside, and giving up master, closing the card or dying commits it again. Debian's Mesa runs on it unmodified (llvmpipe through `kms_swrast`, lavapipe for Vulkan; see [developer/display-core.md](developer/display-core.md#mesa)), with the processor described to it by `compat/linux/cpu.rs` (`/proc/cpuinfo`, `/sys/devices/system/cpu`).
- **Display modes** (`drivers/display.rs`): when the display can switch live (Bochs VGA, virtio-gpu) the resolution changes at once (new framebuffer, every window clamped, a `SCREEN` event to apps, then a 15-second "Keep this resolution?"); a virtio display also follows the host window. Elsewhere the firmware's GOP modes (passed in BootInfo v3) are offered and the choice goes to `\aurora\boot.conf` for the bootloader.
- **Screenshots:** Print Screen or Super+Shift+3 saves a PNG to Pictures, encoded on a background task.

## 5. Storage (`drivers/pci.rs`, `drivers/block/`, `fs/`)

**PCI.**
- Enumerates devices through ECAM, using the base from the ACPI MCFG table, with the legacy `0xCF8` ports as a fallback.
- Sizes BARs (including 64-bit BARs) and walks capability lists.
- BARs above the physical window are mapped uncached with `paging::map_mmio`.

**Block layer.** Drivers implement `BlockDevice` (sector size and count, read, write, flush, and `discard` for TRIM where the medium has it) and register their disks. `block::init` then reads each disk's GPT and registers the partitions as devices of their own. All drivers are **polled**: they copy through physically contiguous DMA bounce buffers (`Dma`) and yield to the scheduler while waiting. Interrupt-driven I/O comes with MSI in M5.

| Driver | Details |
|---|---|
| `ahci.rs` | Resets the HBA, then for each port: FIS receive, COMRESET, and one command slot. Uses IDENTIFY, READ/WRITE DMA EXT and FLUSH CACHE EXT. Empty ports are skipped in about 20 ms. |
| `virtio.rs` | Modern virtio-pci 1.0 (common, notify and device-config capabilities). One split virtqueue with 16 entries; each request is a 3-descriptor chain (header, data, status). |
| `nvme.rs` | Resets the controller, then sets up the admin queue, Identify controller/namespace, and one I/O queue pair. Transfers use PRP1 and PRP2, or a PRP list above 8 KiB. Supports 512 B and 4 KiB LBA formats. |
| `sdhci.rs` | SD cards and eMMC on SD Host Controllers (below). |

**SD cards and eMMC** (`libs/sdmmc`, `drivers/block/sdhci.rs`). The protocols live in a `no_std` library the host tests; the kernel supplies registers, time and DMA memory.
- *Controller* (`host.rs`, SD Host Controller spec 1.00–4.20 in its 3.00-compatible layout): reset, bus power, the card clock (10-bit divided clock from 3.00, power-of-two divider before), bus width, timing (high speed; UHS-I SDR25/50/104 and DDR50, eMMC HS200 and HS400 through Host Control 2, with the card clock stopped while it changes), 1.8 V signalling, and the standard tuning procedure. A command waits for the inhibit bits, clears stale status, writes the argument, then Transfer Mode and Command in one 32-bit store. Data moves through a 256 KiB bounce buffer (below 4 GiB without 64-bit addressing) by PIO, SDMA (answering the 512 KiB boundary interrupt) or ADMA2 with 32- or 64-bit descriptors of up to 32 KiB; DMA addresses are written before the block count, since some controllers (QEMU's) start SDMA when the address is written with a count set. Any error resets the CMD line, and the DAT line too when data or busy was involved.
- *Cards* (`card.rs`): SD bring-up (CMD0, CMD8, ACMD41 asking for high capacity and 1.8 V, CMD11 and the DAT-line handshake, CMD2/3/9/7, SCR, ACMD6, CMD6 switch functions) and eMMC (CMD1 in sector mode, host-assigned address, `EXT_CSD`; the user area selected and boot/RPMB partitions never touched; high-capacity erase groups and the cache on; an 8-bit bus verified by reading `EXT_CSD` back, falling back to 4 and 1; HS400 when allowed, else HS200 with CMD21 tuning, else high speed). Multi-block I/O is bounded by CMD23 when the card has it, else ended by Auto CMD12; writes wait for the transfer state (CMD13). A failure is followed by recovery (line resets, CMD12 if the card is still sending or receiving); the second retunes a tuned bus, the third brings the card up again one speed lower; a different card after that counts as removed. `discard` is TRIM on eMMC (whole erase groups without it) and erase on SD; `flush` empties the eMMC cache.
- *Kernel side*: PCI functions of class 08/05 (a register block per slot in the BARs the Slot Information register names) and ACPI devices with SDHCI ids (registers and interrupt from `_CRS`, found by the ACPI task). Interrupts by MSI, INTx (polled until ACPI routes the line) or an ACPI GSI; the handler acknowledges everything and keeps it in `seen`, which reads of the status registers add back, so the driver core sees statuses as if still pending. Cards present at boot register before the partition scan, so the system can live on one; `boot.conf` (readable only once `/Boot` is mounted) can then force a transfer mode or cap the speed, bringing the card up again if needed. Removable cards show their volumes under `/Volumes` through `block::removable` (shared with USB storage), and an `sdhci` task follows insertion and removal (interrupts, or a look twice a second). After S3 every slot is reset and its card brought up again (a different card is treated as removed).
- *Simulation* (`mock.rs`): a controller and SD/eMMC cards in software that enforce the specifications' rules (clock limits per mode, 1.8 V for UHS and HS200, tuning before tuned modes, inhibit bits, response types, ADMA tables) and record violations, and inject command and data errors on demand. The host tests and the kernel self-test both drive the real code through it.

**VFS mounts.** The longest matching prefix wins:

| Path | Filesystem | Notes |
|---|---|---|
| `/` | AuroraFS on the partition typed `57415645-4653-4175-726F-72612D465331` | Falls back to RamFS when no disk is found |
| `/System` | TarFS over the boot-time `system.tar` | Read-only |
| `/Boot` | FAT32 on the EFI System Partition | Read/write, long file names, case-insensitive |

Disk-backed filesystems sit on `fs/cache.rs`, a write-back cache of 4 KiB blocks with LRU eviction. Its `flush` writes dirty blocks in order, merging adjacent runs, and then flushes the device. A `flusher` kernel task calls `sync_all` every 5 s, and shutdown and restart sync first.

### AuroraFS on-disk format (`libs/aurorafs`)

```
block 0            superblock (magic WAVEFS01, geometry, counts, UUID, label, CRC32); backup in the last block
journal            1/16 of the volume, 32–1024 blocks
block bitmap       1 bit per 4 KiB block
inode bitmap       1 bit per inode (one inode per 16 KiB of disk)
inode table        256-byte inodes: kind, links, size, times, 12 extents + 1 overflow extent block,
                   owner, group and mode (bytes 240–251, with a flag)
data               file contents; directories are files of (inode, kind, name) records
```

**Consistency.** Every metadata change (superblock, bitmaps, inodes, directory contents) goes into the running transaction. The transaction is an overlay of block images that reads also see. `commit` then:
1. Flushes file data, which is written in place ("ordered mode"), so metadata never points at unwritten blocks.
2. Writes a descriptor, the block images and a commit record with a CRC over all of them to the journal, then flushes.
3. Copies the images to their home locations.

On mount, a complete, checksummed transaction left in the journal is replayed, and a torn one is ignored. Freed blocks are not reused until the transaction that freed them commits. Freeing a block also revokes any journaled image of it, so a replay can never overwrite file data that later reused the block.

**Owners** are a compatible feature (`FEATURE_OWNERS`): a checksummed superblock extension (bytes 188–255) holds the feature bits and the volume's *default owner*, which every inode without its own owner belongs to. Older kernels ignore the extension and still mount the volume; inodes they rewrite fall back to the default owner. A volume from before owners gets them at its first boot with them: the default owner becomes the first account (uid 1000), its personal folders become private, and `/Security` and `/Applications` are made.

The same crate formats volumes on the host (`xtask/src/image.rs`). Its tests (`cargo test -p aurorafs`) simulate power cuts by keeping only flushed writes, and check the result with `fsck`.

### FAT32 (`libs/fat32`)

The driver reads and writes FAT32 with VFAT long file names, generating `NAME~N.EXT` aliases. Since FAT has no inodes, the driver assigns stable inode numbers from each entry's location. Its tests cross-check against the independent `fatfs` crate in both directions.

## 6. Devices and power

### USB (`drivers/usb/`)

- **xHCI** (`xhci.rs`). After taking the controller from the firmware (USB legacy support) and resetting it, the driver works from rings of 16-byte TRBs: a command ring, a transfer ring per endpoint, and an event ring on which the controller reports completions and port changes (with an MSI). Device state lives in contexts; the driver fills in input contexts to address and configure devices.
- **Two tasks per controller.** `xhci-events` drains the event ring: it completes waiting transfers, feeds interrupt pipes (key presses, pointer motion, hub changes) to their drivers, and reports port changes. `usb` handles those changes: it resets ports, enumerates new devices (address, descriptors, configuration) and hands them to class drivers, or tears down unplugged ones. Transfers may be issued from any task; the controller lock is held only while TRBs are queued.
- **Class drivers.** `hub.rs` (USB 2 and 3 hubs, with routes and transaction translators), `hid.rs` (boot keyboards; mice and absolute tablets from a report-descriptor parser), `msc.rs` (SCSI over Bulk-Only Transport as a `BlockDevice`; through `block::removable`, FAT volumes on GPT, MBR or whole-disk media mount at `/Volumes/<label>` and are unmounted on unplug or eject), `uac.rs` (USB Audio, below).
- **Isochronous streams** (`iso.rs`, `xhci/iso.rs`). A class driver selects an alternate setting with `iso::set_interface` (the old setting's endpoints leave the controller's schedule with a Configure Endpoint that drops them, the new ones join it, then SET_INTERFACE) and opens a stream on an endpoint with an `IsoHandler`. xHCI keeps `depth` transfer descriptors queued, one per service interval (an Isoch TRB with "start ASAP", plus a Normal TRB if the buffer crosses 64 KiB; TBC/TLBPC for SuperSpeed bursts); endpoint contexts carry the interval (full-speed frames become microframes), Max ESIT Payload, burst (high-speed additional transactions) and an error count of 0. As each descriptor completes, the event task gives IN data to the handler and queues the next one, asking the handler to `fill` OUT data. Since the ring is FIFO, an event for one descriptor retires the ones before it (missed service); ring underruns and overruns retire them all; counters per stream (`IsoStats`) record completions, misses, dry rings and errors. Closing stops the endpoint and moves its dequeue pointer past what was queued. EHCI, OHCI and UHCI answer `IsoError::Unsupported`.
- **USB Audio** (`uac.rs`, UAC 1.0 and 2.0). From the raw configuration descriptor it reads the AudioControl topology (terminals, mixer/selector/feature/processing units, UAC 2 clock sources and selectors) and every AudioStreaming setting. Per streaming interface it picks the PCM setting nearest 48 kHz 16-bit stereo (then 44.1 kHz; 24-bit and mono or multichannel are converted), sets the rate (UAC 1 endpoint control, UAC 2 clock `CUR`, with rates from its `RANGE`), finds the feature unit on the path to the speaker (or from the microphone) and its volume range, and registers an output or input with the sound system. Outputs stream only while the mixer plays to them: packets are sized by a 16.16 accumulator (44/45 frames per ms at 44.1 kHz), and asynchronous endpoints' feedback (10.14 or 16.16, whichever is plausible) adjusts it. Inputs stream while a capture stream is open. An unplug marks the function gone first, so nothing more goes to the device, and unregisters it; the mixer moves on.

### Sound (`drivers/audio/`)

- **Intel HDA** (`hda.rs`). The controller talks to codecs over the CORB/RIRB rings (with the immediate-command registers as fallback). In the audio function group the driver walks the widget graph from every output pin (speaker, headphones, line out) back to a DAC, selects, unmutes and powers that route, and binds every DAC to one output stream: a looping ring of eight 1024-frame buffers at 48 kHz, 16-bit stereo, interrupting (MSI) after each. Headphone jacks with presence detection mute the speakers.
- **Devices** (`mod.rs`). Outputs (`Output`: a ring of 48 kHz stereo frames the device plays in a loop) and inputs (`capture::Input`) register as they appear and leave when unplugged. One output is active: the one chosen this session (menu bar), else the `audio_output` preference (a device name, saved by Settings and the menu bar, so a headset is used again whenever it is plugged in), else a built-in device, else any; `audio_input` picks the default input the same way. `AUDIO_DEVICES` lists them; `convert.rs` converts rates (linear interpolation) and sample layouts (16/24/32-bit, mono, multichannel) for devices that differ from the mixer's format.
- **Mixer** (`mod.rs`). Programs write samples to playback streams (`AUDIO_OPEN` gives a file descriptor with back-pressure). The `audio` task sums the streams, applies the volume and a soft knee, and keeps the active device's ring filled about 64 ms ahead of what it plays (after 3 s with no stream open it rests: the device is silenced and stopped — for HDA the stream's DMA and its interrupts — until a stream opens or a sound plays); when the active device changes it silences (or stops) the old one and starts over on the new one. A device with its own volume control (a USB feature unit) gets the system volume in decibels (`volume.rs`, the same square curve: 40·log10(v/100)) and the mixer makes up whatever the device can't go down to. System sounds are WAV files synthesized at build time into `/System/Sounds`.
- **Capture** (`capture.rs`). An input delivers 48 kHz stereo through `captured`; every capture stream (`AUDIO_CAPTURE`, a readable file descriptor) on it gets a copy, buffering up to a second before dropping the oldest. A device records only while some stream is open on it; when it is unplugged, reads drain what is left and then return 0. The `record` tool writes WAV files.

### ACPI (`acpi/`)

- **Static tables** (`tables.rs`) are parsed early: MADT, FADT, MCFG, HPET, and `\_S5` by a pattern scan as a fallback.
- **The runtime** (`runtime.rs`) runs the `acpi` crate's AML interpreter on its own `acpi` task, with a kernel `Handler` (`handler.rs`: physical memory, ports, PCI configuration, time, AML mutexes). It loads the DSDT and SSDTs, initializes devices, reads the sleep states and finds batteries (`_BIX`/`_BIF`, `_BST`), the power adapter (`_PSR`) and the lid (`_LID`). It then waits for SCI events or a 30-second timer to refresh them.
- **Devices only ACPI describes** (`devices.rs`). At start the task walks the namespace for HID over I²C devices, I²C and GPIO controllers: `_HID`/`_CID`, `_STA`, `_UID`, `_ADR`, `_CRS` (decoded by `aurora-acpi-res`), `_DSM` (the HID descriptor register), `SSCN`/`FMCN` (bus timings), and runs `_PS0`. It hands the result to `drivers::i2c` and logs one `i2c:` line per device. Other tasks run AML later (power states after sleep) through `devices::call`.
- **Isolation.** All AML runs on that task. If the interpreter panics on unusual firmware, the panic handler ends only that task: ACPI features switch off and the system keeps running.
- **The SCI** (`events.rs`) acknowledges the power and sleep buttons and masks fired general-purpose events; the task runs their `\_GPE._Lxx`/`_Exx` methods and re-enables them.

### Idle states and frequency (`power/idle.rs`, `power/cpufreq.rs`, `acpi/processor.rs`)

- **Idle states.** Polling, `hlt` (C1), and with MONITOR/MWAIT `mwait` with a C-state hint: C1, and on Intel the deeper states CPUID leaf 5 enumerates (with cautious latencies), replaced by the firmware's `_CST` where the namespace has one (FFH entries are `mwait` hints, SystemIO ones a P_LVLx port read; C3 through a port is skipped). Under a hypervisor `hlt` is used, since emulated `mwait` usually spins. `idle=halt|mwait|poll` in `boot.conf` overrides.
- **Governor** (like Linux's "menu"): the time to the CPU's next timer, scaled by how long idle periods of that length really lasted, capped by a repeating pattern in the last eight, predicts the idle period; the deepest state whose target residency fits and whose exit latency the energy mode allows is chosen. A CPU polling or in `mwait` watches its own `need_resched` line, so a waker's write wakes it without an IPI.
- **Frequency.** Intel HWP (`IA32_HWP_REQUEST`: full range, autonomous, an energy/performance preference), AMD CPPC in MSRs (Zen 2 and later, `CPPC_REQ` likewise), or ACPI `_PSS` P-states through `IA32_PERF_CTL` / AMD's P-state MSRs with a schedutil-like governor (1.25 × utilization × top frequency, per CPU, when it idles and every 20 ms while busy below the top). Each CPU writes its own MSRs: a change bumps a generation that every CPU applies in its idle loop (and again after sleep or a restart).
- **Energy mode.** Settings → Battery (`energy_mode`: Battery saver, Balanced, Performance), more frugal on battery (the ACPI task reports the adapter): the EPP, the turbo cap (Battery saver on battery), and the idle exit-latency limit (Performance: 100 µs). `_PDC`/`_OSC` are not called: firmware that loads its `_CST`/`_PSS` tables in response uses AML `Load`, which the interpreter doesn't implement.
- `cpuinfo` shows the timer mode, the idle states with each CPU's residency, the frequency driver (and APERF/MPERF frequency where readable), per-CPU wakeups and the tasks whose timeouts woke the system most.

### Sleep (`power/s3.rs`)

1. On CPU 0, after syncing the filesystems and running `\_PTS(3)`, the other CPUs are parked.
2. The IOAPIC routes and the PCI configuration of every device are saved, the FACS waking vector is pointed at the trampoline page, and the CPU context is saved like `setjmp`.
3. `SLP_TYP`/`SLP_EN` power everything down but RAM.
4. On wake, the firmware starts CPU 0 in real mode at the trampoline, which enters long mode and calls `resume_entry`. That reloads the kernel page tables, the GDT and TSS (clearing its busy bit), the IDT, `syscall` and FPU setup, and returns into `suspend` as if the save had just returned.
5. `suspend` then restores the clocks, the APICs, PCI configuration and MSI/MSI-X routes, each driver's hardware (`drivers::resume`: AHCI, NVMe, virtio, HDA, xHCI, network cards, the display devices' committed state and picture, PS/2; I²C controllers and devices are set up again by their own tasks), ACPI events and `\_WAK`, and finally restarts the other CPUs.

## 7. Networking (`drivers/net/`, `net/`)

### Adapters

Every driver implements the `net::Nic` trait (`mac`, `link_up`, `send`, `poll`) and hands received frames to `net::receive`. `drivers/net/mod.rs` matches PCI ids to drivers and names known adapters for `lspci`; [HARDWARE.md](HARDWARE.md#wired-network-adapters) lists which are tested.

- **virtio-net** (`drivers/net/virtio.rs`) uses the shared modern virtio-pci transport (`drivers/virtio.rs`): receive queue 0 with MSI-X, transmit queue 1 whose buffers are reclaimed lazily, a 12-byte header per frame.
- **e1000/e1000e** (`drivers/net/e1000.rs`) runs 256-entry descriptor rings with 2 KiB buffers. The 82574L uses MSI-X (routed through `IVAR`); cards without MSI are polled by the network task. Intel's chipset LAN (I217–I219) is recognized but not driven.
- **igb/igc** (`drivers/net/igb.rs`: 82576, 82580, I350, I210/I211, I225/I226) uses queue 0 with 256 advanced descriptors, MSI-X in extended mode (both queues and link changes on vector 0 through `IVAR`), and restarts auto-negotiation through MDIO when the MAC sees no link after reset.
- **RTL8139** (`drivers/net/rtl8139.rs`): a 32 KiB receive ring the card fills with header-prefixed frames, four transmit slots in turn, 32-bit DMA; its legacy INTx line once ACPI routes it, polled until then.
- **RTL8168/8111/8125** (`drivers/net/rtl8169.rs`): C+ descriptor rings (OWN bit), the generation picked from the chip revision in `TCR`; RTL8168G and later are taken out of out-of-band mode first and reach the PHY through the OCP window. Written from the datasheets; QEMU can't emulate it.

### The stack

- **One task.** `net::init` starts a `net` kernel task that drains the receive queue, processes frames, and runs timers (ARP retries, TCP retransmissions, delayed ACKs, TIME_WAIT, DHCP). It sleeps until a frame arrives, a program sends, or the stack's next timer is due (`next_round`): every 10 ms while a card has no working interrupt or TCP has data to send, 100 ms while DHCP negotiates or ARP/ND wait, otherwise a second (link changes). The stack's state sits behind one mutex. After each round the task bumps a generation counter and wakes waiters, so a socket's wait never holds the stack's lock while sleeping.
- **Layers.** Ethernet and ARP (unresolved packets wait in a queue for up to 3.5 s), IPv4 without fragmentation, ICMP (echo, and unreachable reported to TCP), UDP, and TCP (`net/tcp.rs`): the full state machine, MSS 1460, window scale 3, 256 KiB buffers, out-of-order segments kept, RFC 6298 timers, slow start and congestion avoidance, fast retransmit on three duplicate ACKs, zero-window probes, a 5-second TIME_WAIT and RFC 1337.
- **Interfaces.** `lo` (127.0.0.1) plus one `eth<n>` per adapter. The DHCP client (`net/dhcp.rs`) gets the address, router and DNS servers, and renews at half the lease. The desktop hears about changes and redraws the menu bar.
- **Sockets** (`net/socket.rs`) are file descriptors: TCP (connect, listen, accept, shutdown), UDP datagrams, and ICMP "ping" sockets (the kernel fills in the identifier and checksum). Reads and writes block with a timeout, or return `EAGAIN` in non-blocking mode.
- **Randomness** (`random.rs`) is a ChaCha20 generator seeded from RDSEED/RDRAND and timing jitter, rekeyed after use; it gives TCP its initial sequence numbers and programs `GETRANDOM`.

## 8. The web (`libs/web`, `libs/tls`, `libs/surf`, `userland/apps/surf`)

All three libraries are `no_std` and make no system calls. Programs supply connections, text measurement and images, so the libraries run and are tested on the host.

- **HTTP** (`libs/web`). `http::fetch` follows redirects over any `Connect`or. `corekit::web` provides one that resolves names (DNS over UDP), opens TCP or TLS connections, and keeps idle ones in a small per-process pool (15 s, up to 8), retrying on a fresh connection if an idle one was closed by the server.
- **TLS** (`libs/tls`). The client offers TLS 1.3 and 1.2. In 1.3 it sends an X25519 key share, answers a HelloRetryRequest with P-256, runs the HKDF key schedule, and checks `CertificateVerify` and `Finished`. In 1.2 it does ECDHE with AEAD ciphers, the extended master secret and the downgrade check. Certificates are parsed by our DER reader and validated to one of Mozilla's roots (packed by xtask into `/System/Certificates/roots.bin`): names (wildcards and IP addresses), validity dates, CA flags and signatures (RSA PKCS#1 and PSS, ECDSA P-256 and P-384). The primitives come from RustCrypto.
- **The engine** (`libs/surf`):
  - `html.rs` tokenizes and builds a tree the forgiving way: implied `html`/`head`/`body`, implied end tags, raw-text elements and character references.
  - `css.rs` parses stylesheets (with `@media` and `@supports`) and matches selectors right to left. `style.rs` runs the cascade: the user-agent sheet, presentational attributes, author sheets and `style` attributes, with `!important`, inheritance, custom properties, `calc()`/`min()`/`max()`, and `::before`/`::after` content. Rules are bucketed by id, class and tag, and each element carries a 256-bit filter of its ancestors' ids, classes and tags, so most descendant selectors are rejected without walking the tree.
  - `layout.rs` produces a display list of rectangles, text runs, images and bullets, plus link and form-field areas and `#fragment` anchors. It covers block flow with margin collapsing; inline formatting measured in 1/64 px, breaking only at break opportunities; lists; tables (automatic column widths, spans); flex rows; grid (tracks, areas, spans, auto-placement); floats as rows; and form controls.
- **Learn** lays out the handbook with the engine. The web browser is NetSurf, a separate Linux program in `/System/NetSurf` ([NETSURF.md](NETSURF.md)).

## 9. Telemetry and the System Explorer (`telemetry.rs`, `xtask/src/monitor.rs`)

When QEMU attaches a second UART (COM2), `telemetry::init` detects it with a scratch-register test and the kernel starts streaming newline-delimited JSON:

- **Events:** `stage` (boot progress), `proc` (start, exit, crash), `win` (open, close), `mount`, `dev`.
- **Snapshots:** a `telemetry` task writes one every 250 ms. It carries every task with its CPU time and CPU, the scheduler's switch log since the last snapshot (with the CPU of each switch), per-CPU busy and idle time, per-CPU wakeups (`pcpu`: timer interrupts, slice ends, empty interrupts, expired timers, IPIs and IPIs avoided, time in idle states) and the tasks whose timeouts woke the system (`wakers`), device interrupt vectors, processes, memory, per-syscall and per-IRQ counters, per-device I/O counters, mounted filesystems, and the window list.

Lines are written while holding the port lock and without allocating, so events are safe before the heap exists. Without COM2 the hooks cost one relaxed atomic increment.

`cargo xtask run --monitor` works like this:
- QEMU connects COM2 to a Unix socket owned by xtask, and xtask turns the COM1 console into `console` events.
- xtask serves `docs/explorer/index.html` plus a Server-Sent Events stream at `/events`. The stream replays history, so a page opened late still sees the boot.
- The page is a single self-contained file with no dependencies. With no server it plays `docs/explorer/demo.js`, a recorded session.

## 10. Testing

- **`cargo xtask test`.** It builds the kernel with `--features ktest`. The tests in `kernel/src/tests.rs` cover:
  - the frame allocator and the heap: size classes, buddy runs and the 4 GiB limit, stress from every CPU with cross-CPU frees, running out of memory, and a micro-benchmark (`mm-bench:` lines)
  - integer math and rectangle math
  - the keyboard decoder and RamFS
  - timer accuracy; the timer queue (deadline order, never early, cancelling, timers armed on every CPU fire there); timeouts on CPUs with the tick stopped; and a quiet idle window (at most 50 timer interrupts per second on any CPU, idle time still counted)
  - preemptive scheduling and task reaping
  - wallpaper generation
  - address-space teardown, checked for frame leaks
  - the user-space `usertest` suite, which exercises every syscall: bad pointers, pipes, child processes, exit codes, and a crashing child
  - keyboard layouts, AltGr and dead keys
  - the TrueType engine with the installed faces
  - Spotlight's calculator and file index, the clipboard, and the Trash with Put Back
  - storage: a GPT read, a write-and-verify round trip on the real controller (checking that completions arrive by interrupt), AuroraFS through the kernel adapter on a RAM disk, AuroraFS on the home volume checked with `fsck`, and FAT32 on `/Boot`
  - SD cards and eMMC: every run has a second SDHCI reader (version 2.00) with a 4 GiB card holding a FAT32 volume (`AURORA-SD`). The tests read and write a file through it; write and read back runs across block and bounce-buffer boundaries by PIO, SDMA and ADMA2, each mode reading what the others wrote, and discard; measure sequential and random throughput in every mode against the system disk (`sd-perf:` lines); and pull the card out and put it back (the harness answers `harness: eject sd-card` and `insert sd-card` with QMP `eject` and `blockdev-change-medium`). With `--disk sd` or `--disk emmc` the whole suite runs from a card on a version 3.00 reader (64-bit ADMA2, 8-bit bus); OVMF can't read cards, so xtask copies the card's EFI partition to a small SATA disk that only starts the bootloader. For eMMC the harness puts boot and RPMB partitions filled with known bytes in front of the image and checks after each boot that nothing wrote them. A simulated controller (`aurora_sdmmc::mock`) adds what QEMU lacks: an 8-bit HS200 eMMC with cache and TRIM, injected CRC, timeout and ADMA errors through the block layer, and a UHS-I SDR104 card inserted and removed.
  - SMP: work on every CPU with lock stress, TLB shootdowns, the TSC against the HPET
  - user copies that fault, recovering with `EFAULT`
  - sound: a tone through HDA (the host also checks that QEMU's WAV recording isn't silent); format and rate conversion; the device registry and capture streams with a pretend microphone; and on xHCI a tone through QEMU's `usb-audio` (UAC 1, isochronous; the host checks its own recording of it holds sustained sound, not just a few packets), then the harness unplugs it mid-stream over QMP (`device_del`), the mixer falls back to HDA, and plugs it back (`device_add`), where it is chosen again and plays
  - ACPI: AML, sleep types, the test battery, adapter and lid; and that a failing AML task leaves the system running
  - USB: a hub with a keyboard behind it, a tablet, and a FAT32 stick mounted, read and written
  - I²C: a test SSDT (generated by `xtask/src/i2c_ssdt.rs`) describes a touchpad as laptop firmware does (`_CRS` built with `ConcatenateResTemplate`, a `_DSM`, `_PS0`, an absent alternative model); the self-test build binds its controller to a software bus with a Precision Touchpad (`aurora_i2c::mock`), and the touchpad's swipe, two-finger scroll and tap must reach the input queue
  - sleep: S3 (the harness wakes the VM over QMP), then CPUs, work on every CPU, disk I/O, sound, USB (the USB speaker plays again) and the network after waking
  - networking (on the card `--net` chose, which the test checks is the one in use): DHCP, ping to loopback and our own address, UDP and TCP over loopback (a megabyte each way), a megabyte over HTTP from a server xtask runs on the host, and an inbound connection through a QEMU port forward

  `usertest` also covers threads, futexes and SIMD state surviving preemption, randomized layouts and UMIP.

  `pkgtest` installs, updates and removes apps from a signed test repository the harness serves (`target/test-repo`, under `/repo/`, and an altered copy under `/badrepo/`), and the kernel checks what the desktop sees between the steps.
- **Fuzzers.** `cargo xtask test` runs two system-call fuzzers briefly (see "Hardening" below); longer or reproducible runs take `AURORA_FUZZ_SEED`, `AURORA_FUZZ_ROUNDS`, `AURORA_FUZZ_ITERS` (native) and `AURORA_LINUXFUZZ_CHILDREN`, `AURORA_LINUXFUZZ_CALLS` (Linux), e.g. `AURORA_FUZZ_ROUNDS=200 AURORA_FUZZ_ITERS=5000 AURORA_TEST_FILTER=fuzzer cargo xtask test`. `AURORA_FUZZ_RAM=MiB` leaves only that much memory to programs while they run (the rest held by the test), so they fuzz under memory pressure: swapping, and the out-of-memory killer.
- **Swap.** `swap:` tests (see `mm/swaptest.rs`): private, fork-shared, shared and locked pages swapped out and checked byte for byte; a program (`fuzz swap`) filling three times the memory it may have and checking every page twice; the swap file filling; the out-of-memory killer's choice and a real out-of-memory kill. `cargo xtask test --memory-mib 256` runs them on a small machine; `AURORA_SWAP_TEST_RAM` (MiB left to programs, `all` for all of it) and `AURORA_SWAP_FACTOR` change how much they fill. `mm: what memory costs a program under no pressure` prints `bench:` lines; `AURORA_TEST_KERNEL=path` runs another kernel with this build's programs, to compare them.

  The kernel reports through QEMU's `isa-debug-exit` device. `xtask test` boots the same fresh disk **twice**: the first boot leaves a file, and the second must find it intact.
- **Headless boots.** `cargo xtask run --headless` exposes QEMU's HMP monitor at `target/qemu-monitor.sock` and QMP at `target/qemu-qmp.sock`. That makes it possible to script screenshots and input (`screendump`, `input-send-event`).
- **Host tests.** `cargo test -p aurora-hid -p aurora-acpi-res -p aurora-i2c -p aurora-sdmmc` covers HID descriptors and gestures, ACPI resource decoding (hand-laid descriptors, malformed buffers), the HID over I²C protocol against the software device, the DesignWare driver against a simulated controller (small FIFOs, NACKs, a stuck bus), and the SD/eMMC driver against a simulated SDHCI controller (every transfer mode, SDSC/SDHC/UHS-I/eMMC bring-up, HS200 and HS400, boot partitions, TRIM and erase, injected errors and fallbacks, removal and write protect). `cargo test -p aurora-lz4` checks the LZ4 compressor and decompressor (round trips of every kind of page, the format's end-of-block rules, interoperability with the reference `lz4` tool where it is installed, and millions of mutated blocks the decoder must refuse or decode without reading or writing out of bounds; `AURORA_MUTATIONS=N` for more). `cargo test -p aurorafs -p fat32 -p lumen -p aurora-image -p aurora-wav -p nebula-web -p nebula-secure -p nebula-engine -p xtask` covers the filesystem libraries (crash recovery, `fatfs` cross-checks), the TrueType engine (metrics and placement against fontdue, coverage against 4× supersampling), the image codecs (against the `png`, `jpeg-encoder`, `jpeg-decoder` and `gif` crates), WAV decoding, URLs and HTTP (redirects, keep-alive, gzip), TLS (every cipher suite and version against a rustls server, RFC 8448 key schedule, bad certificates), the browser engine (parsing, selectors, the cascade and layout) and the AML assembler for the test SSDT.
- **Scripted UI.** `tools/qmp.py` clicks, types and takes screenshots in a running VM.
- **CI.** On every push, CI runs the host tests, the kernel suite on AHCI, virtio-blk, NVMe and an SD card with 4 CPUs (and AHCI with 1 CPU), with virtio-net (and e1000e and RTL8139 on one job each), and a headless boot to the desktop.

## 11. Hardening

Each CPU feature is enabled only where CPUID reports it, on the boot CPU, on every other CPU as it starts, and again after waking from S3 (`arch::protect::init`). The boot log's `protections:` line says what is on.

- **SMEP** — the kernel never executes a user page. **SMAP** — the kernel never reads or writes one, except in the user-copy routine (`syscall/user.rs`), which brackets its `rep movsb` with `stac`/`clac`; every trap entry (`arch::trap`) clears RFLAGS.AC again, and `syscall` masks it. All user memory goes through that routine: `put`/`get`/`slice`/`read_u32` and the Linux layer, which uses them for signal frames, `ioctl`s and the rest; the loaders and the vDSO write through the physical window. **UMIP** — `sgdt`, `sidt`, `sldt`, `smsw`, `str` fault in ring 3. **CR0.WP** and **EFER.NXE** are always on.
- **W^X kernel.** `kernel/linker.ld` page-aligns `.text`, `.rodata` + `.got`, and `.data`/`.bss`; `protect_kernel` maps them RX, R+NX and RW+NX, and marks every other kernel PML4 slot no-execute (the physical window is an alias of the code). The bootloader still maps the image RWX until then (it runs before the kernel enables NXE). The AP/S3 trampoline's own page table maps low memory RWX while CPUs start.
- **Kernel stacks** of tasks (`mm::kstack`) have an unmapped 128 KiB guard below each; an overflow ends in a double fault that names it. (CPU boot stacks and IST stacks are heap memory without guards.)
- **Stack protector.** `-Z stack-protector=strong` (`.cargo/config.toml`) with a canary from RDSEED/RDRAND and the TSC, written first thing in `_start` (`stackguard.rs`). About 1,600 functions carry it; `.text` grows by ~104 KiB (3.8 %). The C half (LinuxKPI) is built without it.
- **ASLR** (`mm::aslr`, ChaCha20 from `random.rs`): the `mmap` base (28 bits of pages), the stack top (20 bits, plus 8 bits of 16-byte offset), and for Linux programs the position-independent executable (24 bits) and `ld.so` (27 bits); the vDSO moves with the stack. `aslr=off` in boot.conf turns it off. WaveOS programs are still linked at `0x40_0000`. Plan for them: build userland with `relocation-model=pic` and `static-position-independent-executables` in `userland/x86_64-aurora-user.json`, link `corekit/user.ld` at 0, and have `proc::load_waveos` load at a random base and apply the `R_X86_64_RELATIVE` entries of `PT_DYNAMIC` (no other relocation types appear in a static PIE).
- **Speculation.** Nothing is enabled: no IBRS/IBPB/STIBP, no KPTI, no `verw` on return to user mode. The log line reports which of IBRS, STIBP, SSBD and MD_CLEAR the CPU offers.
- **Fuzzers.** `userland/bin/fuzz` (`fuzz run`) calls every native system call with structured random arguments (null, kernel, non-canonical and straddling pointers, read-only code, boundary lengths, recycled descriptors) on four threads while a fifth unmaps their buffers, closes their descriptors and starts threads; each round is a child process the parent kills if it lingers, and processes blocked in pipes, futexes, sleeps, sockets and `wait` must die when killed. `linux-tests/linuxfuzz.c` does the same with Linux numbers (and `WAVE_BASE` calls) in forked children. Both keep to their sandboxes (`/fuzz`, `/tmp/lfz`) and never make calls that change the machine (power, clock, display, preferences) or reach other processes. The self-test checks that no frames leak. A round may run long (a worker blocked in a pipe it holds both ends of, or in a 20 s connect) while its chaos thread keeps mapping memory and starting threads; that takes all the memory programs may have, which the kernel must survive. `fuzz hog` (several at once, in the "memory exhaustion" self-test) takes everything a program can — threads, full pipes and loopback connections, windows, mapped memory — and the kernel must keep its reserve and get it all back.
