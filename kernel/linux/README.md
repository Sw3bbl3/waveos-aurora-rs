# Linux drivers on WaveOS

This directory lets WaveOS run GPU drivers written for Linux, unmodified,
the way FreeBSD's drm-kmod does. It is compiled with clang by
`kernel/build.rs` and linked into the kernel; the Rust half lives in
`kernel/src/linux/`.

| Path | What | License |
|---|---|---|
| `include/waveos/`, `waveos/` | **LinuxKPI**: the Linux kernel interfaces (memory, locking, work queues, ID maps, red-black trees, printf, devices, DMA fences, virtio), written for WaveOS | MIT |
| `include/linux/`, `include/asm/`, `include/video/`, `include/trace/` | One-line headers mapping Linux's include paths onto `include/waveos/` (plus `linux/hdmi.h` from Linux) | MIT |
| `drm/drm_*.c`, `drm/*.h`, `include/drm/` | The DRM core and KMS helpers from Linux 6.12 | MIT / HPND (per file) |
| `drm/virtio/` | Linux's virtio-gpu driver (6.12) | MIT / HPND |
| `video/hdmi.c` | HDMI infoframes from Linux 6.12 | MIT |
| `include/uapi/` | DRM and virtio protocol headers from Linux 6.12 | MIT, BSD-3-Clause |
| `drm/waveos_*.c` and the headers marked "WaveOS" | WaveOS replacements for the parts Linux licenses GPL-only (managed resources, GEM page-backed objects, GEM framebuffers, the simple encoder) and for services WaveOS leaves out (sysfs, writeback, sync objects), plus the in-kernel display client and the self-test | MIT |

Only files whose own notice permits it are imported: every copied file keeps
its original copyright and license text. Where Linux's implementation is
GPL-only (`drm_gem_shmem_helper`, `drm_managed`, `drm_gem_atomic_helper`,
`drm_gem_framebuffer_helper`, `drm_simple_kms_helper`, `rbtree`, `dma-fence`,
`dma-resv`, ...), WaveOS has its own implementation of the same interface,
and the GPL text is not used.

## How a driver runs

1. `linux::init` (Rust) starts the `linux-worker` thread, which runs the
   drivers' work items and timers and delivers virtqueue completions.
2. `drm_core_init` (Linux's module init) sets up DRM.
3. For each virtio GPU, Rust negotiates features over WaveOS's virtio-pci
   transport, builds the Linux `pci_dev` and `virtio_device`, and calls the
   driver's `probe`.
4. `waveos_display.c`, an in-kernel DRM client (like Linux's fbdev
   emulation), probes the connectors, sets each one's preferred mode and
   hands the compositor a framebuffer; the compositor's damage becomes
   `drm_client_framebuffer_flush` calls.

Try it with `cargo xtask run --gpu virtio`. `cargo xtask test --gpu virtio`
also checks that the display came up. The kernel test "LinuxKPI" runs
`drm/waveos_selftest.c` on every test boot.

## Not yet

3D acceleration (virgl/venus) needs a Linux-compatible user-space interface
for Mesa; the driver's ioctls are compiled in but nothing in WaveOS calls
them yet. Real hardware drivers (amdgpu, i915/xe) also need TTM, the DRM GPU
scheduler, firmware loading and power management.
