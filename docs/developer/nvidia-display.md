# Nvidia display: what exists, and what a real driver needs

For engineers thinking about an Nvidia GeForce driver for WaveOS. **There is no Nvidia display driver.** `kernel/src/drivers/gpu/nvidia.rs` (over `libs/nvgfx`) recognises GeForce GPUs from Maxwell (GTX 900) to Ada (RTX 40), logs what they are, and leaves the display to the firmware framebuffer with the display core's software pointer, exactly as before.

## What runs today

- PCI class 0x03, vendor 0x10DE. The device id gives the architecture (approximately: Nvidia's ids are not laid out by chip; `aurora_nvgfx::arch_of_device`).
- Unless `nvidia=off` is in boot.conf, and only when memory decoding is already on (as the firmware leaves a GPU it uses), one 32-bit read of `NV_PMC_BOOT_0` (BAR 0 offset 0, the identification register documented by envytools and present on every Nvidia GPU since NV04) gives the exact chipset (28:20) and revision (7:0); `aurora_nvgfx::chipset` names it (GM107 … AD107).
- The log line: `nvidia: 01:00.0 [10de:2684]: Ada Lovelace AD102 (RTX 40, chipset 0x192 rev 0xa1); no native display driver (its display needs the GSP firmware); the firmware framebuffer stays, with a software pointer`.
- Nothing is ever written: not PCI configuration space, not a register.

## Why not a takeover like the Intel and AMD drivers

Those drivers keep the firmware's mode and only write a surface address, because Intel and AMD scan out from a handful of documented, double-buffered registers. Nvidia's display engine is not programmed that way:

- **Maxwell and Pascal (EVO).** The display engine (PDISP) is driven through *channels*: a core channel and one base (primary plane) and overlay channel per head, each fed with methods from a push buffer in memory that the display engine fetches by DMA, with context DMA objects describing memory, and `UPDATE` methods that commit state at vblank. A flip is a method sequence on the base channel (surface context DMA, offset, `UPDATE`), not a register write. Taking over means owning the channels the GOP used: tearing down or adopting its core channel, allocating instance memory, push buffers and context DMAs, and knowing the per-chip channel control registers (`0x610xxx`, `0x64xxxx` user areas). Nouveau's display code does this, and resets the display when it starts (a flicker) rather than adopting the firmware's channels.
- **Volta to Ada (NVDisplay).** The same idea with *windows*: a core channel, window and window-immediate channels, heads, precomposition and LUT state, and different method classes per generation (C37D, C57D, C67D, C77D…). The class headers are public (NVIDIA's open-gpu-doc and open kernel module sources); the register-level channel setup and the ordering rules are not documented for outside use.
- **Turing onwards (GSP).** Nvidia's own driver hands the GPU to the GSP firmware (a RISC-V core running GSP-RM). On Ada it must: the kernel driver's only path to the display is RPC to GSP-RM, which owns the hardware. Nouveau supports Ada only through GSP.

A safe takeover therefore needs, at minimum, channel management and memory objects that a single wrong write can turn into a hung GPU or a hung PCIe bus. That is not something to write from guesses and ship as the default on machines nobody has tested; detection, an honest log line and the firmware framebuffer are the safe choice.

## What a real driver needs

In order, each a project of its own:

1. **Memory.** VRAM management through BAR 1 (the aperture, often resizable) and the GPU's MMU (page tables per channel / per VM; instance memory). Context DMAs (EVO) or page-table-backed surfaces (NVDisplay, GSP).
2. **GSP boot (Turing, Ampere, Ada).** Load the GSP-RM firmware (`gsp-<version>.bin`, Nvidia's redistributable files as shipped in linux-firmware for nouveau), version-locked to the RPC ABI. On Ampere and Ada: run FWSEC from the VBIOS to set up the write-protected region (FRTS), start the booter (signed, on SEC2), which loads and verifies GSP-RM into WPR2, then start the RISC-V core. Turing differs in the booter details. Every step is signed-firmware handshakes with status polling; errors are opaque.
3. **RM client.** The message queues in shared memory (command and status queues, headers, sequence numbers), `GSP_RM_ALLOC`/`GSP_RM_CONTROL`/`FREE` RPCs, and the object hierarchy (client, device, subdevice, display common, display class) with the control commands' parameter structures of that exact firmware version.
4. **Display.** Through RM: allocate the display object, query the heads and the connectors (`NV0073` controls: active displays, DP AUX and link training are RM controls too), allocate core and window channels and their push buffers, read the state the VBIOS lit, then flip by window channel methods and `UPDATE`. Hot-plug arrives as RM events. Pre-GSP (Maxwell, Pascal): the same EVO channels programmed directly, after the VBIOS's devinit (already run by the firmware at boot); display does not need signed firmware there, but everything else (PMU, graphics) does.
5. **DRM uAPI for Mesa's NVK.** NVK targets nouveau's current uAPI: `GETPARAM` (chipset, VRAM size, GART, BAR sizes, exec push limits), `GEM_NEW`/`GEM_INFO`/mmap, `CHANNEL_ALLOC`/`CHANNEL_FREE` per engine class, `VM_INIT`, `VM_BIND` (sparse and regular binds, synchronous or with syncobjs), `EXEC` (push buffers with wait/signal syncobjs, timeline syncobjs), PRIME/dma-buf export and import, and KMS on the same device. NVK supports Maxwell onwards, but without GSP (which also does reclocking) pre-Turing GPUs run at boot clocks and are slow.

On WaveOS the display part would plug into the display core like the others (a `KmsDevice`: CRTC per head, primary plane per window, cursor via the cursor channel or window), and steps 1–3 and 5 belong to a GPU layer under the DRM node other agents are building.

## Risks and the honest estimate

- Firmware ABI churn: every GSP-RM release changes structures; a driver pins one version.
- Opaque failures: a wrong step in the boot chain leaves the GPU unusable until a power cycle; a wrong display channel write can hang the bus.
- Testing needs the hardware: QEMU has no Nvidia GPU and no simulated GSP is realistic.

A minimal display-only driver for Turing to Ada (GSP boot, RM client, display channels, flips, cursor) is a multi-month effort with hardware in hand. Maxwell/Pascal direct EVO is smaller but serves GPUs that are 8–10 years old.

## Testing

- `cargo test -p aurora-nvgfx`: device ids of common GeForce cards (GTX 970 to RTX 4060) map to their architectures, `NV_PMC_BOOT_0` values decode to the right chips.
- On real hardware: boot normally and send the `nvidia:` line from `\aurora\boot.log`. The desktop should be the firmware framebuffer at the bootloader's resolution with a software pointer, as before.
