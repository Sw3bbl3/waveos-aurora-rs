# WaveOS Aurora architecture

This document is for someone about to read or change the code. It follows the machine from power-on to the desktop, then covers each subsystem.

## Repository layout

| Path | Crate | Target | Role |
|---|---|---|---|
| `bootloader/` | `aurora-boot` | `x86_64-unknown-uefi` | UEFI application: loads the kernel |
| `kernel/` | `tide` | `x86_64-unknown-none` | The kernel, compositor and apps |
| `libs/bootinfo/` | `bootinfo` | both | `#[repr(C)]` handoff contract |
| `xtask/` | `xtask` | host | Build, run, image and test orchestration |
| `assets/fonts/` | — | — | TTFs rasterized at build time |

The workspace's `default-members` is only `xtask`, so a plain `cargo build` never tries to build the kernel for your host. `rust-toolchain.toml` pins nightly, because the kernel uses `abi_x86_interrupt`, and installs both bare-metal targets.

## 1. Boot: `aurora-boot`

`bootloader/src/main.rs` runs as a UEFI application (`EFI/BOOT/BOOTX64.EFI`):

1. **Graphics.** It opens the Graphics Output Protocol and picks 1280×800 if the firmware offers it. Otherwise it picks the largest mode no wider than 1920 pixels.
2. **ACPI.** It finds the RSDP in the UEFI configuration table.
3. **Kernel.** It reads `\aurora\kernel.elf` from the boot volume. Our own ELF reader (`elf.rs`) walks the `PT_LOAD` segments. They are copied into one physically contiguous allocation, with `.bss` zeroed.
4. **Page tables** (`paging.rs`), a fresh 4-level hierarchy with three mappings:
   - all RAM, the framebuffer and the low 4 GiB (APIC MMIO) at **`PHYS_OFFSET = 0xffff_8000_0000_0000`**, using 2 MiB pages
   - the same range identity-mapped, so the loader survives the CR3 switch
   - the kernel image at its link address **`0xffff_ffff_8000_0000`**, using 4 KiB pages
5. **Handoff.** It allocates a 512 KiB kernel stack and a `BootInfo` page, then calls `ExitBootServices`. The UEFI memory map is translated into `bootinfo::MemoryRegion`s and adjacent regions are merged. Then it runs `cli; mov cr3; mov rsp; call _start` with `rdi = &BootInfo`.

Everything the loader allocates is `LOADER_DATA`. That memory is reported as `MemoryKind::Bootloader`, so the kernel never hands it out.

## 2. Kernel bring-up (`kernel/src/main.rs`)

```
serial → GDT/TSS → IDT → mm (frames, heap, drop identity map) → ACPI → APIC
      → PS/2 + IRQ routing → RamFS → scheduler → sti → spawn "crest" → idle
```

### Memory layout

| Virtual range | Contents |
|---|---|
| `0x0000_0000_0000_0000 …` | unmapped, so null derefs fault (reserved for user space in M2) |
| `0xffff_8000_0000_0000 + phys` | physical memory window (`mm::phys_to_virt`) |
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

## 3. Crest compositor (`kernel/src/gui/`)

- **Main loop.** The compositor is the `crest` task (`gui/mod.rs`). It drains input, lets the `Desktop` update, and repaints only the **damaged rectangles** into a RAM back buffer. It then copies those rectangles to the GOP framebuffer, swizzling pixels for RGB framebuffers.
- **Canvas** (`canvas.rs`): a clipped 32-bit ARGB rasterizer, all integer math.
  - It provides rounded rectangles, anti-aliased by integer-sqrt distance, and polylines.
  - It provides gradients, glyph blending, drop shadows computed from a signed distance field, and "glass".
- **Glass.** The wallpaper is box-blurred once at startup. Menu-bar, dock and launcher pixels sample that blurred copy under a tint, which gives real frosted glass with no per-frame blur cost.
- **Wallpapers** (`wallpaper.rs`): procedural, with a star field, aurora curtains and layered anti-aliased waves. They use an integer sine table generated by `build.rs`.
- **Fonts.** `kernel/build.rs` rasterizes Inter (Regular and SemiBold) and JetBrains Mono at fixed sizes on the host, using fontdue. The kernel embeds the alpha masks, so it has no TTF parser and does no floating-point rasterizing. This is also why the panic screen can draw text without a heap.
- **Desktop** (`desktop/mod.rs`): the window stack (z-order is Vec order), focus, drag and resize, double-click zoom, and minimize. It also routes input to apps.
- **Shell** (`desktop/shell.rs`): the menu bar, dock, launcher and dropdown menus.
- **Apps** (`apps/`): implement the `App` trait, which has `draw`, `key`, `click`, `hover`, `scroll` and `tick`. Apps ask the desktop to do things by pushing `Request`s, such as open an app, open a file, close, power off, or change the theme. Apps never touch the window manager directly, and this boundary is what moves behind IPC in Milestone 2.

## 4. Testing

- **`cargo xtask test`.** It builds the kernel with `--features ktest`. The tests in `kernel/src/tests.rs` cover:
  - the frame allocator and the heap
  - integer math and rectangle math
  - the keyboard decoder and RamFS
  - timer accuracy
  - preemptive scheduling and task reaping
  - wallpaper generation

  The kernel reports through QEMU's `isa-debug-exit` device.
- **Headless boots.** `cargo xtask run --headless` exposes QEMU's HMP monitor at `target/qemu-monitor.sock` and QMP at `target/qemu-qmp.sock`. That makes it possible to script screenshots and input (`screendump`, `input-send-event`).
- **CI.** It does both on every push.
