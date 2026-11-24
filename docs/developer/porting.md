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
