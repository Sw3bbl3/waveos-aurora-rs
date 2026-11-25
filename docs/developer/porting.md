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

Arm entries: `PRESENT` = valid + page/table + inner shareable + PXN (bits
always set together, so `vmm`'s `e & (PRESENT | USER) == PRESENT | USER`
tests work; the kernel never executes what it maps at run time); `USER` =
AP[1]; `NO_EXECUTE` = UXN; `ACCESSED` = AF; `HUGE` is software bit 58,
which the bootloader sets on its 2 MiB blocks (a block's low bits, `0b01`,
can't be tested with a mask). Software bits: 55-57 and 59-61
(`SOFT_0..5`; with hardware attributes off, 59-62 are ignored by the MMU),
**`WRITABLE` = 63 and `DIRTY` = 62** (62 is also `SOFT_NOT_PRESENT` in
invalid entries). Memory types (MAIR_EL1): 0 normal write-back, 1 device
nGnRE (`NO_CACHE`), 2 normal non-cacheable (`WRITE_COMBINING`), 3 device
nGnRnE.

**Write permission and dirtiness are software bits on Arm** (Apple's
cores under HVF have no hardware dirty-bit management, FEAT_HAFDBS).
`vmm` writes every leaf entry through `pte::leaf` (an identity on x86),
which sets AF and derives the hardware read-only bit AP[2]: a kernel page
is writable as soon as `WRITABLE`, a user page only once also `DIRTY`. The
first write to a clean writable page, and any access after the reclaim
scan cleared AF, fault once; `trap` resolves them at once (interrupts
masked, no lock: `mmu::fix_soft_fault`, a compare-and-swap) as a CPU with
hardware management would. Write-protecting a page (fork's copy-on-write,
`mprotect`) goes through `leaf` and sets AP[2]. `leaf` also makes a frame
that becomes user-executable fetchable (`cache::sync_code`, see below).

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
arm64 Linux's numbers (the Linux layer's table: "The Linux layer's
ABIs").

**User entry:** `enter_user(entry, stack, arg)` starts EL0t with `x0 =
arg`, every other register zero, SP_EL0 = `stack` (the loader's stack top,
16-byte aligned for native programs; corekit aligns it again), PSTATE = 0
(interrupts unmasked, NZCV clear). TPIDR_EL0 is 0 unless the process set a
thread pointer (`sched`'s "FS base"; native code doesn't use it). New
threads and forked children start from a full register frame
(`enter_user_frame`). Pointer authentication: the APIA key is set and
SCTLR_EL1.EnIA on, so `paciasp`/`autiasp` in user code sign and check
(one key per boot, shared by every process for now); the other keys
(IB, DA, DB, GA) are off, so instructions using them are NOPs. BTI landing
pads are not enforced (no guarded pages). EL0 may read CTR_EL0, use
`dc zva` and cache maintenance by address (`dc cvau`, `ic ivau`: JITs sync
their own code), and `wfi`/`wfe`.

**FP/SIMD:** v0-v31, FPSR and FPCR, eagerly saved per task in Linux's
`user_fpsimd_state` layout (528 bytes). SVE stays off for EL0.

### Ending a test run

QEMU's `virt` has no isa-debug-exit device. `arch::power::qemu_exit(code)`
prints `[aurora-exit <code>]` on the console and powers off with PSCI
SYSTEM_OFF; the test runner reads the code from the serial log (QEMU's
own exit status is 0). This works the same under HVF and TCG, which
semihosting does not promise.

### Interrupts, CPUs, protections

- **GICv3** (`gic.rs`) from the MADT (distributor, redistributors, each
  CPU's MPIDR) and GTDT (the virtual timer's PPI, 27 on `virt`). Group 1,
  priority 0xA0, EOI mode 0. SPIs route to the boot CPU. SGI 0
  reschedules, 1 is a TLB shootdown, 2 halts (panic). MSIs go through the
  GICv2m frame (`msi=gicv2m`: HVF's GIC has no ITS): a slot's MSI is a
  write of its SPI to the frame's doorbell. GICv2 is not driven (drivers
  poll there).
- **Timer:** the EL1 virtual timer (CNTV), one deadline at a time
  (CNTV_CVAL is the clock's own count); `fired` masks it before EOI.
- **SMP:** PSCI `CPU_ON` for each MADT CPU at `aurora_ap_entry`
  (physical, MMU off) with a start block (MAIR, TCR, SCTLR, TTBR1 = the
  kernel root, TTBR0 = a 1 GiB-block identity map of the kernel image,
  stack, CPU index in TPIDR_EL1), cleaned to the point of coherency. TLB
  maintenance is broadcast (`tlbi vaae1is`); `vmm`'s shootdown IPIs are
  SGIs too.
- **Protections:** PAN (SPAN cleared; the copy routine clears PSTATE.PAN
  around its loop, `.inst`-encoded since the kernel targets ARMv8.0), PXN
  on all user and run-time kernel mappings, the kernel image W^X, user
  pointer authentication (above). Not yet: BTI enforcement, kernel PAC
  (needs `-Zbuild-std` for the kernel: the prebuilt `core` lacks branch
  protection), ASIDs (TTBR0 switches flush the TLB).
- **Instruction cache:** Apple's cores report CTR_EL0.IDC = DIC = 0, so
  code the kernel writes for user space must be cleaned to the point of
  unification and the I-cache invalidated (`cache::sync_code`, by address
  through the physical window: the I-cache is PIPT).

### Status (milestone 2)

Works under HVF on 1 and 4 vCPUs: interrupts, the timer, SMP, PCI through
ECAM, virtio-blk and virtio-net on MSI-X (DHCP), xHCI with the USB
keyboard, tablet, storage and audio, HDA, SD cards (sdhci-pci), ACPI AML,
the desktop on `ramfb`, native programs at EL0, Linux programs (the
static arm64 tests: files, memory, threads, clocks with the vDSO,
processes and signals, sockets, input devices). `cargo xtask test --arch
aarch64` passes on 1 and 4 vCPUs under HVF; `tests::ARCH_SKIPS` lists what
it skips and why: the C half of the kernel (LinuxKPI), a PC's ACPI sleep
states and SSDT devices (test battery, I2C touchpad), S3, and the DRM
device tests (the display is ramfb, not a PCI GPU).

### Status (milestone 3: daily use under HVF)

- The gate adds `tools/smoke_desktop.py` (first-run setup, apps, typing,
  a window drag, Shut Down, login on the next start) to the full suite.
- Hardware virtualization runs the guest 15-60 times faster than TCG and
  exposed races that emulation hid. A late interrupt could take a waiter
  off a `WaitQueue` before it slept, and the next interrupt then found
  nobody to wake. A disk request then looked lost for its 5 s timeout,
  and fsync under load stalled (power-cut stress: 41 synced files in 30
  cuts before, 577 after). QEMU's usb-storage leaves a status request
  that comes before its SCSI command has finished unanswered; the driver
  now reads the status again instead of resetting the device.
- Start to desktop: 1.3 s from launching QEMU (it was 5.9 s, 5.4 s of
  them EDK2's boot manager waiting for a key; `-boot
  menu=on,splash-time=0` on the `virt` machine sets that wait to zero).
  The kernel reaches the desktop 0.3-0.4 s after firstlight starts it.
- Idle at the desktop (headless, logged in): QEMU uses about 0.02 s of
  host CPU a minute. The timer is one-shot, an idle CPU waits in `wfi`,
  and nothing wakes it periodically. (With a display window the host's
  display refresh adds its own cost, not measured here.)
- Address-space switches still drop the whole TLB (`tlbi vmalle1is`, no
  ASIDs): 0.5-1 us each against 40-50 ns without a flush (`mm: what
  switching address spaces costs (bench)`). ASIDs need user leaf entries
  marked nG and an ASID retired when its root frame is freed (a new
  address space can get the same frame).
- The USB audio check records only the stream after the test's replug:
  QEMU's wav file starts over when the device comes back. Its length is
  the test's 600 ms window under HVF (about 880 ms of wall time under
  TCG), against the 400 ms floor. The guest sends every packet, 48
  frames per millisecond.

- The wake-up fix above was completed afterwards: with the counter alone
  a waiter could still read it just after `wake_all` raised it and join
  the list just before `wake_all` emptied it. `wake_all` now leaves
  waiters on the list and each leaves it itself after its sleep; "wait
  queues: a wakeup just before the waiter sleeps doesn't lose the next"
  fails without it (round 85 on x86 with the counter alone). The same
  stall had made browser starts 5 s or 10 s longer at random.
- QEMU's window on macOS shows nothing from virtio-gpu's cursor plane, so
  `xtask run` sets `cursor=software` there and the compositor draws the
  pointer (`--hardware-cursor` keeps the plane).

Not yet: the C half of the kernel (x86-only builds), the PCI I/O window,
the second PL011 (telemetry), ASIDs, per-process pointer-authentication
keys.
The Linux layer's arm64 ABI (call table, signal frames, vDSO) is in place:
see "The Linux layer's ABIs".

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
for arm64; the layer's own arm64 ABI is described in "The Linux layer's
ABIs" below.

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

## The Linux layer's ABIs

`kernel/src/compat/linux` implements every Linux call once; what differs
between the x86-64 and arm64 Linux ABIs is in `compat/linux/abi/` and in
the data crate `libs/linuxabi` (no kernel dependencies, so the host tests
check it).

- **One numbering, one table per ABI.** The shared `call()` dispatches by
  x86-64 numbers (`abi::nr`, the layer's own numbering). An ABI's
  `lookup(number)` says what a program's number is: `Call::Shared(nr)`,
  `Call::Own` (the ABI's handler) or `Call::None`. x86-64 is the
  identity. arm64's is `aurora_linuxabi::aarch64::TABLE`: the generic
  table (Linux 6.1: 306 calls), each mapped onto the x86-64 call of the
  same name — every arm64 call has one — except `clone`, whose last two
  arguments are the other way round (`tls` before `child_tid`) and is
  `Own`. The paused i386 branch's table (`ia32/table.rs`: `Same(nr,
  widening)`, `Compat(fn)`, `No`) is the same scheme; it can move to
  `aurora_linuxabi::{Entry, Map, Table}` and `abi/` with its widening
  spec added to `Map::Same`.
- **Memory laid out per ABI, not translated.** `struct stat`
  (`abi::native::put_stat`: arm64's is 128 bytes with 32-bit
  `st_nlink`/`st_blksize`), `struct epoll_event` (`EPOLL_EVENT_SIZE`,
  `EPOLL_DATA_AT`: packed 12 bytes on x86-64, 16 on arm64), the open flags
  that differ (`O_DIRECTORY`, `O_NOFOLLOW`; also `O_DIRECT`,
  `O_LARGEFILE`), `MINSIGSTKSZ` (2048 / 5120), `uname`'s machine and
  `AT_PLATFORM` (`MACHINE`). Everything else the layer reads or writes —
  `flock`, `statfs`, `termios`, `stack_t`, `siginfo`, the kernel's
  `sigaction`, signal numbers, `SA_*`, `MAP_*`, `CLONE_*`, `AT_*`,
  ioctl numbers, errno — is the same on both, which the host tests check
  value by value.
- **Signals.** `compat/linux/signal.rs` keeps the shared machinery
  (actions, masks, pending sets, which signal goes where, restarting);
  `abi::native::signal` provides `Regs`, `enter_handler`, `sigreturn` and
  `restart`. arm64's frame is Linux's: a frame record `{x29, x30}`, then
  the `rt_sigframe` (`siginfo`, `ucontext` with `sigcontext` and a
  4096-byte record area holding `fpsimd_context`, an `esr_context` for
  faults and the terminator); the handler gets x0-x2, sp, x29 = the
  record and x30 = the vDSO's `__kernel_rt_sigreturn` (or `SA_RESTORER`'s
  address). `rt_sigreturn` checks sp alignment, PSTATE (EL0t, AArch64,
  DAIF clear), every record (an unknown one, an `extra_context` or SVE
  record, or a missing `fpsimd_context` ends the program with SIGSEGV, as
  on Linux) and the alternate stack in `uc_stack`. A call interrupted by
  a signal is made again by `pc -= 4` and x0 = the call's first argument,
  which `syscall::dispatch` passes to `signal::on_return` (Linux's
  `orig_x0`).
- **vDSO.** `kernel/vdso/vdso-aarch64.c` (built by `kernel/build.rs` for
  `aarch64-unknown-linux-gnu`, linked with `rust-lld`): `__kernel_
  clock_gettime`, `__kernel_gettimeofday`, `__kernel_clock_getres`
  (version `LINUX_2.6.39`, as glibc and musl look them up) read CNTVCT_EL0
  and the same time page as x86's; `__kernel_rt_sigreturn` is exactly
  `mov x8, #139; svc #0` with Linux's CFI and a preceding `nop`, which
  libgcc and libunwind recognise. Unwind tables stay in the image
  (`PT_GNU_EH_FRAME`).
- **Start-up.** ELF loading is shared (static, static-PIE, dynamic through
  `/lib/ld-linux-aarch64.so.1`, the same ASLR layout); arm64 adds
  `AT_HWCAP2` and `AT_MINSIGSTKSZ` (4720: the frame, the record, 16 bytes
  of alignment) after the common entries, and `AT_PLATFORM` is
  `aarch64`. TLS is TPIDR_EL0 (`sched`'s thread pointer), set by
  `clone(CLONE_SETTLS)`; there is no `arch_prctl`.
- **CPU features.** `aurora_linuxabi::aarch64::cpu` derives `AT_HWCAP`,
  `AT_HWCAP2` and `/proc/cpuinfo`'s `Features` line from the ID
  registers as Linux 6.1 does, never reporting SVE, SME, MTE or BTI (the
  kernel doesn't switch their state or guard pages yet), PACA/PACG only
  with user keys, CPUID only when ID-register reads are emulated; and
  `emulate` gives the sanitised value of an EL0 `mrs` of an ID register
  (MIDR as it is, MPIDR's RES1 bit, the visible feature fields, 0 for
  unallocated ones). `abi::aarch64::emulate_mrs(&mut TrapFrame)` decodes
  the trap (EC 0x18, or EC 0 with an `mrs` at pc), writes Xt and steps
  over it.
- **Checked against Linux's headers.** `cargo xtask linux-abi --arch
  <arch>` fetches Debian 12's `linux-libc-dev` and `libc6-dev` for the
  architecture (through the pinned snapshot, hashes checked), has clang
  evaluate every `__NR_*`, the layouts and the constants (nothing runs,
  so any host works) and writes `libs/linuxabi/tests/headers/<arch>.txt`;
  `cargo test -p aurora-linuxabi` asserts the tables, encoders and frame
  code against them.

### What the Linux layer gets from `arch/`

1. **ID registers:** `arch::aarch64::cpu::id_regs()` returns
   `aurora_linuxabi::aarch64::cpu::IdRegs` (MIDR, ID_AA64PFR0/1,
   ISAR0/1/2, MMFR0/1/2), read once on the boot CPU at start-up;
   `abi::aarch64::id_regs()` returns it.
2. **`mrs` emulation:** `trap::sync` calls
   `compat::linux::abi::aarch64::emulate_mrs(f)` for EC 0x18 and EC 0
   from EL0 of a Linux process before raising SIGILL, and returns to the
   program when it returns true.
3. **EL0 counter access:** `cpu::init_el0` sets CNTKCTL_EL1.EL0VCTEN on
   every CPU (the vDSO reads CNTVCT_EL0; CNTFRQ_EL0 is always readable).
4. **Instruction cache:** SCTLR_EL1.UCI/UCT/DZE are set (EL0 `dc cvau`,
   `ic ivau`, CTR_EL0, `dc zva`). The kernel cleans the D-cache to PoU and
   invalidates the I-cache (`cache::sync_code`, only where CTR_EL0 says it
   is needed) whenever a user page becomes executable: every leaf entry
   `vmm` writes goes through `pte::leaf`, which does it for a user page
   without UXN — new mappings (file pages from the page cache included),
   `mprotect` to PROT_EXEC, copy-on-write copies, swap-in — and when the
   loaders write code into an already executable page (`vmm::write_bytes`).
   A program that writes code into its own executable page (a JIT) syncs
   it itself, as on Linux.
5. **TPIDRRO_EL0** is zeroed on every CPU (`cpu::init_el0`); the kernel
   never writes it. **TPIDR_EL0** is the program's: `sched` reads it back
   (`arch::cpu::user_thread_pointer`) when a user task leaves its CPU, and
   `sched::fs_base` returns the live register in a program, so `fork`,
   `clone` and switches keep what the program set with `msr`.
6. **Pointer authentication: APIA only.** One APIA key per boot, the same
   in every process; EnIB, EnDA and EnDB stay off (those instructions are
   NOPs) and there is no GA key, so the Linux layer must not advertise
   PACA/PACG. Per-process keys (switched with the address space) are a
   follow-up.
7. `orig_x0`: not in `TrapFrame`; the Linux layer takes it from
   `syscall::dispatch`'s saved arguments, which covers system calls.
8. **Returning to a new program:** the exception exit loads SP_EL0 from
   any frame whose saved PSTATE is EL0t, so a handler that rewrites its
   frame (`execve`, with `TrapFrame::user_start`) returns on the new stack.

Still open: BTI guarded pages; per-process pointer-authentication keys.

### Linux programs on aarch64: state

Under HVF (`cargo xtask test --arch aarch64`), the static musl tests pass:
linuxtest, proctest, nettest, inputtest, linuxfuzz, sandboxtest and the
sandboxed linuxfuzz, and with `AURORA_TEST_LINUX_ROOT=1` (a root from
`cargo xtask linux-root --arch aarch64 --base curl`) Debian's dynamically
linked GNU Hello and curl fetching from the test server. drmtest runs
with `--gpu virtio` (virtio-gpu-pci, a real PCI device) and is skipped on
`--gpu std` (ramfb has no PCI device behind it). musl's `vfork` is `fork`
on arm64 (proctest's memory-sharing check is x86-only; `posix_spawn` makes
the CLONE_VM | CLONE_VFORK call on both).

Mesa (`cargo xtask test --arch aarch64 --gpu virtio --mesa`, Debian's arm64
Mesa in `target/mesa-root-aarch64`): kmscube at the display's 60 Hz,
glmark2 (desktop GL and GLES), vkcheck on lavapipe, es2gears in a window
through wl-bridge with dma-bufs, vkcube, and the compositor's zero-copy and
direct-scanout test all pass.

The browsers (`AURORA_TEST_ROOT=browsers`, the full arm64 root from `cargo
xtask linux-root --arch aarch64`): Voyage (WebKitGTK) and Firefox ESR start
and show pages, local and real (Wikipedia, YouTube); the existing browser
measurement (copies against zero-copy) and the start-up and page-load
timings (`AURORA_TEST_FILTER="page loads timed"`) pass. See
docs/LINUX.md for the numbers against x86 under TCG.

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
- The desktop as a person uses it: `tools/smoke_desktop.py OUTDIR --arch
  aarch64` (after `cargo xtask build --arch aarch64`) boots twice on a fresh
  disk and drives everything over QMP: first-run setup typed in, Terminal
  running commands, Files, Settings, Notes with a note typed, a window
  dragged, Shut Down from the menu; then the login screen with the saved
  password and the note and folder still on disk. About 80 s under HVF;
  `--pace 0.2` presses keys and moves on five times faster (races),
  `--smp 1` on one processor.
- CI: `host-arm64` runs the host tests on an arm64 runner (the NEON
  paths); `aarch64` builds, lints and boots the aarch64 system under TCG.
