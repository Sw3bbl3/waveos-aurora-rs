# Porting: the architecture layer and the aarch64 port

WaveOS Aurora runs on x86-64 and is being brought up on 64-bit Arm
(AArch64), first on QEMU's `virt` machine under Apple's Hypervisor.framework
(`-accel hvf`) and TCG. This page is the reference for the boundary between
the kernel and the processor (`kernel/src/arch/`) and for the aarch64
conventions user space and the Linux layer build on. Keep it current with
the code.

## The boundary

Everything that differs between processors lives in
`kernel/src/arch/<arch>/` (`x86_64/`, `aarch64/`). `arch/mod.rs` re-exports
the one that is being built, so the rest of the kernel writes
`crate::arch::cpu::cycles()` and gets the right one. No file outside
`arch/` uses inline assembly, `core::arch` intrinsics, port I/O, MSRs,
control registers or the `x86_64` crate (which is now an x86-only
dependency). x86-only drivers outside `arch/` (PS/2, the VMware mouse, the
CMOS clock's registers, `power::cpufreq`'s MSRs) are `cfg`-gated and reach
the hardware through `arch::x86_64::…` or `arch::io`.

Every architecture provides these modules with the same names and
signatures:

| Module | Provides |
|---|---|
| `cpu` | `brand`, `vendor`, `model`, `feature_names`, `hypervisor`, `invariant_counter`, `cycles` (TSC / CNTVCT), `halt` (`hlt` / `wfi`), `halt_forever`, `frame_pointer`, `set_thread_pointer` (FS base / TPIDR_EL0), `linux_hwcap`, `mwait_supported`, `mwait_substates`, `monitor_mwait` |
| `interrupts` | `enable`, `disable`, `are_enabled`, `without_interrupts`, `enable_and_hlt` |
| `irq` | `alloc(name, handler, arg) -> Option<u8>` (a handler slot, "vector"), `route_gsi`, `mask_gsi`, `msi_message(vector) -> (address, data)`, `send_ipi(cpu, Ipi::{Reschedule, TlbShootdown})`, `total`, `list` |
| `percpu` | `PerCpu` (`lapic_id` = the CPU's hardware id: APIC id or MPIDR affinity; `cr3` = page-table root), `CPUS`, `MAX_CPUS`, `cpu_id`, `this`, `count`, `set_count`, `online`, `init` |
| `timer` | the per-CPU deadline timer: `Mode`, `mode`, `local_mode`, `choose`, `start_local`, `program(deadline_ns)`, `fired`, `always_running`, `writes`, `describe` |
| `clock` | `NAME`, `counter_hz()`: the cycle counter's rate when it can be the clock (x86: calibrated TSC; Arm: CNTFRQ_EL0) |
| `switch` | `switch(save_sp, load_sp)`, `init_stack(top, f, arg)`, `SAVED_FRAME_POINTER`, `SAVED_RETURN` |
| `fpu` | `FpuState` (`new`, `save`, `restore`, `as_bytes`, `from_bytes`), `size`, `xfeatures`, `init` |
| `syscall` | `SyscallFrame` (`number`, `args`, `set_return`, `ret`, `pc`, `sp`, `set_sp`, `user_start(pc, sp)`), `init`, `set_kernel_stack`, `enter_user(entry, stack, arg)`, `enter_user_frame(&frame)` |
| `trap` | `TrapFrame` (`user_mode` and the accessors above) |
| `uaccess` | `copy(dst, src, len) -> 0/1` (the only code touching user memory), `fixup(pc, addr)` |
| `mmu` | `pte::*` entry bits, `root`, `load_root`, `flush_page`, `flush_all`, `enable_features`, `write_combining` |
| `smp` | `start_aps(cpu_ids, trampoline)`, `halt_others` |
| `protect` | `init`, `report`, `protect_kernel(root)`, `smap`, `features` (and test hooks) |
| `rng` | `random`, `seed`, `early_seed` (register-only, for the stack protector) |
| `cache` | `wc_fence` (`sfence` / `dsb st`), `full_fence`, `flush_line` (`clflush` / `dc civac`), `write_back_all` |
| `io` | `Port<T>` (x86 port I/O; on Arm the PCI I/O window, all ones until one is set) |
| `power` | `off`, `reset(acpi_reset_port)`, `qemu_exit(code)` |
| top level | `early_init`, `EARLY_INIT_DONE`, `init_interrupts(&acpi)`, `route_legacy_input(&acpi)` |

x86-only modules (`apic`, `gdt`, `idt`, `pat`, `msr`, `s3`) are reached as
`arch::x86_64::…`; aarch64-only ones (`psci`, `regs`) as `arch::aarch64::…`.

### Page tables

`mm::vmm` walks four levels of 512 entries with `(va >> (12 + 9 * level))
& 0x1FF` (level 3 the root) on both architectures, and names entry bits
through `arch::mmu::pte` (`PRESENT`, `WRITABLE`, `USER`, `ACCESSED`,
`DIRTY`, `HUGE`, `NO_EXECUTE`, `NO_CACHE`, `WRITE_COMBINING`, six
software bits `SOFT_0..5` and `SOFT_NOT_PRESENT`, `ADDR`, `KERNEL_TABLE`,
`USER_TABLE`). One root page holds both halves on both: on Arm, TTBR1
(T1SZ = 16) uses entries 256..512 for the kernel half and TTBR0 (T0SZ =
17, the 47-bit user space below `USER_END`) only entries 0..256, so the
kernel half that `vmm` copies into every process's root is ignored there
and `mmu::load_root` changes TTBR0 alone. The virtual layout is the same
on both (docs/ARCHITECTURE.md, "Memory layout").

Arm entries: `PRESENT` = valid + page/table + access flag + inner
shareable + PXN (the kernel never executes what it maps at run time);
`USER` = AP[1]; `NO_EXECUTE` = UXN; `HUGE` is software bit 58, which the
bootloader sets on its 2 MiB blocks (a block's low bits, `0b01`, can't be
tested with a mask); software bits are 55-57 and 59-61 (with hardware
attributes off, 59-62 are ignored by the MMU), and 62 in invalid entries.
Memory types (MAIR_EL1): 0 normal write-back, 1 device nGnRE (`NO_CACHE`),
2 normal non-cacheable (`WRITE_COMBINING`), 3 device nGnRnE.

**Milestone 1 limitation:** `WRITABLE` and `DIRTY` are 0 on Arm (every
mapping is writable, nothing tracks dirtiness). Read-only and copy-on-write
user pages need the software write bit with AP[2], which comes with user
programs.

## The aarch64 port

### Boot

- **Firmware:** EDK2 (`edk2-aarch64-code.fd`) with ACPI. `cargo xtask
  build|run --arch aarch64` builds `firstlight` for `aarch64-unknown-uefi`
  (`EFI/BOOT/BOOTAA64.EFI`) and the kernel for
  `aarch64-unknown-none-softfloat`: the kernel is soft-float as on x86, so
  only user FP/SIMD state is switched. Graphics: `--gpu std` is `ramfb`
  (EDK2's virtio-gpu GOP has no linear framebuffer; the loader then boots
  without one).
- **Loader → kernel:** the same `BootInfo` as on x86. The loader builds one
  translation-table root: the physical window at `PHYS_OFFSET` in 2 MiB
  blocks (RAM normal memory, the framebuffer non-cacheable, the rest of the
  low 4 GiB device memory), an identity map of the same range, and the
  kernel image at `KERNEL_BASE` in 4 KiB pages; it cleans the kernel image
  to the point of unification and invalidates the I-cache. Then with
  DAIF masked:
  - at EL1: MMU off, MAIR/TCR/TTBR0/TTBR1 (both the root)/TPIDR_EL1 = 0,
    TLB invalidated, SCTLR_EL1 = `0x34D5_D81D` (MMU, caches, alignment of
    SP, EL0 cache maintenance and `dc zva`), SP = the boot stack, `br`
    to `_start`;
  - at EL2 (QEMU with `virtualization=on`; some firmware): HCR_EL2.RW (and
    E2H cleared), timers and GICv3 system registers open to EL1, no FP
    traps, the same EL1 registers, then `eret` to `_start` at EL1h.
  `x0` = `&BootInfo` (in the physical window). The kernel drops the
  identity map (`mm::paging::init`).
- **Kernel start:** `arch::early_init` sets TPIDR_EL1 (CPU index), the
  vector table (VBAR_EL1), FP/SIMD access (CPACR_EL1.FPEN); `time` uses
  CNTVCT_EL0 at CNTFRQ_EL0; `psci` takes the conduit (`hvc`/`smc`) from the
  FADT's ARM_BOOT_ARCH flags; the console is the PL011 at `0x0900_0000`.

### Exceptions and the system call convention

All exceptions enter through `arch/aarch64/trap.rs`, which saves a
`TrapFrame` (x0-x30, SP, ELR, SPSR, ESR, FAR, vector kind; 304 bytes) on the
kernel stack (SP_EL1, which is the running task's stack whenever it is in
user mode, so no TSS-like switch is needed) and calls `trap`.

**Native system calls:** `svc #0`; number in **x8**, arguments in
**x0-x5**, result in **x0** (a negative `aurora_abi::err` value on failure);
every other register, x8 included, is preserved. The numbers are the same
as on x86-64 (`libs/abi`). Linux programs use the same registers with
arm64 Linux's numbers (that table is the Linux layer's, still to come).

**User entry:** `enter_user(entry, stack, arg)` starts EL0t with `x0 =
arg`, every other register zero, SP_EL0 = `stack`, PSTATE = 0 (interrupts
unmasked). The thread pointer is TPIDR_EL0 (`sched`'s "FS base").

**FP/SIMD:** v0-v31, FPSR and FPCR, eagerly saved per task in Linux's
`user_fpsimd_state` layout (528 bytes). SVE stays off for EL0.

### Ending a test run

QEMU's `virt` has no isa-debug-exit device. `arch::power::qemu_exit(code)`
prints `[aurora-exit <code>]` on the console and powers off with PSCI
SYSTEM_OFF; the test runner reads the code from the serial log (QEMU's
own exit status is 0). This works the same under HVF and TCG, which
semihosting does not promise.

### Status (milestone 1)

Works, under HVF and TCG, entered at EL1 or EL2: the loader, the kernel's
early set-up, memory management (frames, heap, kernel mappings, kernel
stacks), ACPI tables, the clock, PSCI, the VFS with the system image, the
boot screen on `ramfb`, the scheduler's start; then the kernel reports the
CPU and powers off (`arch::aarch64::bringup_done`), before enabling
interrupts.

Not yet: the GIC (interrupts, IPIs, MSI through GICv2m), the generic timer
interrupt, secondary CPUs (PSCI CPU_ON), user programs (EL0 is entered by
`enter_user`, untested), PAN/PXN of the kernel image/W^X, PAC/BTI (needs
`-Zbuild-std` for the kernel: the prebuilt `core` lacks branch
protection), user page permissions (see "Milestone 1 limitation"),
signals for Linux programs (`compat::linux::signal` builds x86 frames only;
on arm64 a handled signal ends the program), the Linux system call table
for arm64, the vDSO and the C half of the kernel (x86-only builds), the
PCI I/O window and ECAM, the second PL011 (telemetry), devices.

## Native user space on aarch64

Every program in `userland/` builds for aarch64 (`cargo xtask userland
--arch aarch64` builds user space alone; `build --arch aarch64` puts it in
the system image).

- **Target:** `userland/aarch64-aurora-user.json`: EL0 code for ARMv8.0
  with NEON (FP/SIMD is always there for user programs), unaligned access
  allowed (unlike the kernel's target), static, no red zone. Core and
  alloc are rebuilt for it (`-Zbuild-std`), into `target/user-aarch64`, so
  user space can use `-Z branch-protection=bti,pac-ret`: return addresses
  are signed (PAC) and indirect-branch targets marked (BTI). Both are hint
  instructions, free on processors without them; BTI is enforced only
  once the kernel sets the guarded-page bit in user mappings, which it
  doesn't yet.
- **System calls:** `corekit::sys::syscall6`, the convention above.
- **Start-up:** `corekit/src/start.rs` is the one place that depends on
  the state EL0 starts in. Its `_start` aligns SP to 16 bytes, zeroes x29
  and x30 (the end of the frame-pointer chain) and calls the program's
  `__aurora_start` (from `corekit::entry!`) with x0, the argument block,
  untouched. Threads start at their function with the argument in x0.
  Native code uses no thread pointer; TPIDR_EL0 belongs to Linux programs.
- **CPU features:** EL0 can't read the ID registers, so programs read the
  `Features:` line of the kernel's CPU report (`aurora_abi::report::CPU`,
  `has_features`), once, on the first question. corekit installs that as
  the detector of `vendor/cpufeatures` (see its WAVEOS-PATCHES.md), which
  the RustCrypto crates ask: AES-GCM then uses the AES and PMULL
  instructions where the processor has them (`--cfg aes_armv8 --cfg
  polyval_armv8`) and their constant-time portable code otherwise.
  ChaCha20 uses NEON (`--cfg chacha20_force_neon`). SHA-256 and Poly1305
  stay portable: sha2 0.10's ARMv8 code needs a C compiler.
- **SIMD:** the compositor's pixel loops (`lumen::simd`: blend, shade,
  cross-fade) have NEON versions beside the SSE2 ones, and video frames
  are converted to ARGB with NEON (`nebula_media::pixels`). Both compute
  the same 16- and 32-bit sums as the scalar code and keep the same bits,
  so pixels are identical on both architectures; host tests compare them
  with the scalar code (natively on an Apple-silicon Mac, SSE2 on x86 CI or
  under Rosetta). A soft-float kernel has no SIMD paths: there the
  functions handle no pixels and the scalar code does all of them.
- **Not architecture-specific:** Pulsar is an interpreter (no JIT); f64
  arithmetic is IEEE on both and Rust never fuses operations, so results
  match; NaN bit patterns differ (x86's default NaN is negative, Arm's
  positive), which nothing observes (`Map` keys canonicalise NaN). The
  H.264 and AAC decoders are portable Rust on both (rusty_aac's own SIMD
  needs `std`). memcpy and friends come from `compiler_builtins` on both.
- **Packages:** a GINA manifest's `architecture` is `x86_64` or `aarch64`;
  the executable's ELF machine must agree, and WaveOS refuses packages for
  the other architecture ("This app needs a different processor
  architecture"). Host tools read both. `uname`, `neofetch`, Nebula's
  `navigator.userAgent` and the WaveSDK use `aurora_abi::ARCH`.
- **usertest:** its UMIP check becomes "an EL0 read of SCTLR_EL1 stops the
  program" on aarch64.

## Linux programs for arm64 (groundwork)

What the Linux layer needs from outside `kernel/src/compat/linux` exists
for arm64; the layer itself (arm64's system call table, signal frames,
the auxiliary vector) is still to come.

- **The system image's Linux programs** build for arm64 with `--arch
  aarch64` (`cargo xtask linux-programs --arch aarch64` builds them alone):
  BusyBox (Debian's `busybox-static`, arm64), the Wayland bridge for
  `aarch64-unknown-linux-musl` (its system calls: `svc #0`, arm64's
  numbers, `clone`/`ppoll`/`unlinkat` where arm64 has no `fork`/`poll`/
  `unlink`), the C tests in `linux-tests/` and Doom, with clang
  (`--target=aarch64-linux-musl`), Debian's arm64 `musl-dev` and the Rust
  toolchain's `compiler_builtins` for 128-bit `long double`. In the C
  sources: `waveos.h`'s `wave_call` uses `svc #0` and x8; `linuxtest`
  checks the NEON registers where x86 checks YMM; `proctest` uses `udf`,
  `brk` and x9/x10 and skips integer division by zero (it doesn't trap on
  Arm); `linuxfuzz` leaves out the calls only x86-64 has. libdrm's
  `modetest` and NetSurf are x86-only so far, and so is the Mesa test disk.
- **The Debian root** (`cargo xtask linux-root --arch aarch64`, into
  `target/linux-root-aarch64`) comes from the arm64 index of the same
  pinned snapshot and `Release`; `--base` takes only libc, BusyBox and GNU
  Hello. Debian's architecture name, the multiarch triplet
  (`aarch64-linux-gnu`), musl's triplet and glibc's loader
  (`/lib/ld-linux-aarch64.so.1`) all come from `xtask/src/arch.rs`.
  `run`/`image --arch aarch64` put it on the disk as `/Linux` with the
  same flags as on x86 (`--linux-root`, `--no-linux-root`; by default when
  it can be fetched).

## Tools

- `xtask/src/arch.rs` holds everything that differs per architecture in
  the build; `xtask/src/virt.rs` the `virt` machine's QEMU line.
- The launcher's Processor choice, per-processor option tables and
  presets: `xtask/src/runconfig.rs` (`available_on`, `builtin_presets_for`,
  `options_json`'s `by_arch`).
- Disks: `aurora/arch`, `refresh_esp`'s refusal, `cargo xtask migrate-disk`
  (`xtask/src/migrate.rs`), see BUILDING.md.
- Repositories and updates per architecture: store-and-updates.md,
  "Architectures".
- CI: `host-arm64` runs the host tests on an arm64 runner (the NEON
  paths); `aarch64` builds, lints and boots the aarch64 system under TCG.
