# AMD display driver (`kernel/src/drivers/gpu/amd`, `libs/amdgfx`)

For engineers working on the AMD Radeon display driver. It is a display core driver (see [display-core.md](display-core.md)); rendering (the graphics engine, command submission, Mesa's radeonsi/RADV) is not part of it.

**Status: written from AMD's published register headers and checked against a simulated display engine; it has not run on real hardware.** It never sets a mode. Every step before the decision to take over only reads registers, and the decision requires the hardware's state to match the framebuffer the bootloader was handed exactly, so the worst outcome on a machine it misjudges should be the firmware framebuffer path WaveOS had before. The one assumption that is not checked by readback is that the VRAM just above the firmware framebuffer is unused (see [VRAM](#vram)).

## Layout

| Where | What |
|---|---|
| `libs/amdgfx` (`aurora-amdgfx`, `no_std`, host-tested) | Device table (`platform`), register layouts per display block (`regs`, generated `layout.rs`), firmware readback and the takeover decision (`takeover`), flips, frame counter and cursor (`scanout`), VRAM placement (`vram`), the DP AUX engine (`aux`), HPD sense (`hpd`), and `mock`, a simulated DCN/DCE engine |
| `kernel/src/drivers/gpu/amd/mod.rs` | The `KmsDevice`: probe, VRAM scanout buffers, flips, dirty copies, cursor, connectors and hot-plug, `amd=` policy |
| `kernel/src/drivers/gpu/amd/hw.rs` | BAR 5 (registers) as `Mmio`, the mapped part of BAR 0 (VRAM aperture) as `Aperture` |
| `kernel/src/drivers/gpu/amd/selftest.rs` | The driver against `aurora_amdgfx::mock` in `cargo xtask test` |
| `tools/amd-regs.py` | Regenerates `layout.rs` from AMD's register headers (`asic_reg/dcn/dcn_*_offset.h`, `asic_reg/dce/dce_11_2_d.h`, `dce_12_0_offset.h`) |

Shared with `libs/igfx`: the `Mmio` trait, the DisplayPort side of AUX (`aurora_igfx::aux::Transport`: DEFER retries, DPCD, I²C-over-AUX, EDID blocks), the simulated DP sink (`aurora_igfx::mock::Sink::handle`), the address-space allocator (`gtt::Space`), `timing::Timings` and `plane::premultiply`. The kernel driver is generic over `Hw: Mmio + Aperture`, so the self-test runs the real takeover, flip, dirty, cursor and hot-plug code against the simulated engine and a RAM buffer standing in for VRAM.

## Coverage

| Block | ASICs | Register segment bases |
|---|---|---|
| DCE 11.2 | Polaris 10/20/30, 11/21, 12 | absolute offsets (no bases) |
| DCE 12 | Vega 10, 12, 20 | from `vega10/vega20_ip_offset.h` |
| DCN 1.0 | Raven, Picasso | UNCONFIRMED (assumed as Vega 10's) |
| DCN 2.0 | Navi 10, 12, 14 | `navi1x_ip_offset.h` |
| DCN 2.1 | Renoir, Lucienne, Cezanne, Barcelo | `renoir_ip_offset.h` |
| DCN 3.0, 3.0.1, 3.0.2, 3.0.3 | Navi 21/22, Van Gogh, Navi 23, Navi 24 | `sienna_cichlid`, `vangogh`, `dimgrey_cavefish`, `beige_goby_ip_offset.h` |
| DCN 3.1.2 | Rembrandt | `yellow_carp_offset.h` |
| DCN 3.1.4, 3.1.5, 3.1.6 | Phoenix/Hawk Point, Raphael/Granite Ridge, Mendocino | UNCONFIRMED (IP discovery) |
| DCN 3.2, 3.2.1 | Navi 31/32, Navi 33 | UNCONFIRMED (IP discovery) |

Every header that prints the DCN/DCE segment bases gives the same ones (0x12, 0xC0, 0x34C0, 0x9000, 0x02403C00). ASICs without such a header get theirs from the IP discovery table in VRAM, which the driver does not parse; it assumes the same values and logs `register bases assumed (UNCONFIRMED)`. If they differ, the readback finds nothing that matches the firmware framebuffer and the driver declines.

Device ids are in `libs/amdgfx/src/platform.rs` (single ids for Polaris, Vega and the APUs, the 32-id blocks AMD gives each Navi 2x/3x ASIC). Not covered: DCE 6–10 (Southern Islands to Fiji), DCE 11.0 (Carrizo, Stoney), Vega M, DCN 3.5+ (Strix and newer APUs), DCN 4 (Navi 4x). They are logged with the reason and left to the firmware framebuffer.

## Boot options

| `boot.conf` | Effect |
|---|---|
| (nothing) or `amd=takeover` | Keep the firmware's mode; flip between WaveOS's VRAM buffers, hardware cursor, AUX EDID, hot-plug |
| `amd=modeset` | Logged and otherwise the same as the default: mode setting is not implemented |
| `amd=off` | Don't bind: the firmware framebuffer (GOP) as before |
| `cursor=software` | (existing) also leaves the cursor registers alone |

## Probe and takeover (`takeover::read`, `Amd::new`)

1. PCI class 0x03, vendor 0x1002, in the device table. BAR 0 (VRAM aperture) and BAR 5 (registers) must be memory BARs, BAR 5 large enough for the layout's highest offset, memory decoding on (the driver never writes PCI configuration space). The bootloader's framebuffer must lie inside BAR 0: on a machine with two GPUs only the one showing the boot screen is considered, and the other is logged and left alone.
2. Read-only readback, per timing generator:
   - DCN: `OTG_CONTROL.OTG_MASTER_EN`, `OTG_INTERLACE_CONTROL`, `OTG_H/V_TOTAL`, `_BLANK_START_END`, `_SYNC_A`, `_SYNC_A_CNTL`; then every HUBP whose `DCHUBP_CNTL` isn't blanked and whose `HUBP_VTG_SEL` names this OTG, with a non-zero viewport and address: `DCSURF_SURFACE_CONFIG` (format, rotation, mirror), `DCSURF_TILING_CONFIG.SW_MODE`, `DCSURF_SURFACE_CONTROL` (DCC, TMZ), `DCSURF_FLIP_CONTROL` (update lock, flip type), `DCSURF_SURFACE_PITCH`, `DCSURF_PRIMARY_SURFACE_ADDRESS(_HIGH)`, `DCSURF_PRI_VIEWPORT_START/DIMENSION`.
   - DCE: `CRTC_CONTROL.CRTC_MASTER_EN`, the CRTC timing registers; DCP *i*'s `GRPH_ENABLE`, `GRPH_UPDATE` (lock), `GRPH_CONTROL` (depth, format, array/swizzle mode, address translation), `GRPH_SWAP_CNTL`, `GRPH_PITCH`, `GRPH_X/Y_START/END`, `GRPH_SURFACE_OFFSET_X/Y`, `GRPH_PRIMARY_SURFACE_ADDRESS(_HIGH)`, `GRPH_FLIP_CONTROL`.
3. Declines (`takeover::Decline`, logged as `amd: declining: …`): no running timing generator; interlaced; nonsensical timings; no plane; a tiled, compressed (DCC), protected (TMZ), rotated or mirrored surface, or one not ARGB8888 (format code 8 on DCN; 32 bpp, format 0, no channel swap on DCE); a page-translated surface (DCE); the firmware holding the update lock; a plane not starting at the surface's corner, or split pipes that don't tile the screen side by side or scan different surfaces; a bootloader framebuffer that isn't BGRX.
4. **The fingerprint:** every lit display must scan a surface of exactly the bootloader's size and pitch, and all of them the same address. Otherwise: `declining: OTG n scans WxH with a pitch of P pixels, not the firmware framebuffer` (or `the displays scan different surfaces`). Wrong register offsets would fail here.
5. The pixel clock: from a DP display's EDID (the timing with the same totals) when one display and one HPD pin are lit, else measured from the frame counter over six frames, else 60 Hz assumed. It only names the mode; nothing is programmed from it.
6. VRAM: two scanout buffers for the first display (see below), else `declining: not enough of the VRAM aperture`.

Only then does it write: the cursor registers (setup, read back), and from the first commit on, surface addresses.

## VRAM

Surfaces stay in VRAM, where the firmware's was; the driver never touches the memory controller, GART or page tables. The surface address of a buffer at aperture offset *o* is the firmware framebuffer's address plus (*o* − the framebuffer's offset in BAR 0): the aperture maps VRAM linearly, so this needs neither the FB location registers nor the system aperture. Buffers come from just above the firmware framebuffer (1 MiB aligned) up to three quarters of the aperture, at most 512 MiB: AMD's firmware keeps its own reservations (PSP trusted memory, VBIOS scratch, the IP discovery table) at the top of VRAM. **That the space above the framebuffer is free is an assumption** (nothing on the GPU runs: no engine is started; amdgpu likewise reserves only the firmware framebuffer and the top-of-VRAM areas). Only the used part of BAR 0 is mapped, write-combining.

## Scanout

- Framebuffers are system RAM like everywhere in the core. Each lit display gets two VRAM buffers of the firmware's pitch (the first display's at takeover, others when a client first gives them a picture). A commit with a new framebuffer copies it whole into the buffer not on screen, then flips: `DCSURF_PRIMARY_SURFACE_ADDRESS_HIGH` then the low half (the low write arms the update, applied at the next vertical update; DCE: `GRPH_PRIMARY_SURFACE_ADDRESS_HIGH`/low, at vblank unless the firmware set horizontal-retrace flips), on every pipe of a split display, and waits up to 100 ms for `SURFACE_FLIP_PENDING` (bit 31 on DCN 1.0, bit 8 from DCN 2.0) or `GRPH_SURFACE_UPDATE_PENDING` to clear. The flip event's sequence is `OTG_STATUS_FRAME_COUNT` (`CRTC_STATUS_FRAME_COUNT`).
- `dirty` copies the damaged rectangles into the buffer on screen (as the GOP path does; this can tear inside one frame), then `sfence` and a read through the aperture.
- Format, pitch, viewport, scaler, watermarks and clocks are the firmware's and stay valid: same size, format and pitch.
- Lit displays nobody gave a picture of their own (the firmware's clones) are pointed at the same buffer.
- `panic_framebuffer` is the VRAM buffer on screen.

## Cursor

64×64 ARGB with premultiplied alpha, in two VRAM slots (alternated so an update never shows half an image), only on displays scanned by a single pipe (split displays get the software pointer). DCN: `CURSOR_SETTINGS` (DST_Y_OFFSET 0, CHUNK_HDL_ADJUST 3), `CURSOR_SIZE` 64×64, `CURSOR_CONTROL` (mode 2, pitch 64, 8 lines per chunk), the DPP's `CNVC_CUR0_CURSOR0_CONTROL` (mode 2, no expansion), `CURSOR_SURFACE_ADDRESS(_HIGH)`; shown with `CURSOR_POSITION`, `CURSOR_HOT_SPOT`, `CURSOR_ENABLE` and `CUR0_ENABLE`. DCE: under `CUR_UPDATE.CURSOR_UPDATE_LOCK`, `CUR_CONTROL` (mode 2), `CUR_SIZE` (63, 63), address, `CUR_POSITION`, `CUR_HOT_SPOT`, `CURSOR_EN`. Positions can't be negative: a cursor hanging off the top or left is placed at 0 with the hot spot moved into the image by as much. The setup reads `CURSOR_SIZE`/`CURSOR_CONTROL` back; if they didn't keep their values the cursor is left off and the pointer drawn in software.

## Connectors, EDID, hot-plug

- One connector per lit display (always connected, the firmware's mode) and one per HPD pin that has ever sensed something and isn't known to be a lit display's (connected while sensed, modes from its EDID, no CRTC: it can't be lit).
- Which pin and AUX channel belong to which display is in the VBIOS's object table, not parsed. When exactly one display is lit and exactly one HPD pin senses something, they are paired: the lit display's connector gets that pin's EDID and kind.
- HPD: `DC_HPD_INT_STATUS.DC_HPD_SENSE` per pin, polled every second by the `amd` task (and a re-probe every 8 s); a change posts `Event::Hotplug`. No interrupt is enabled.
- DP: a DPCD read on AUX channel *n* (HPD pin *n* + 1) tells DP apart; EDID over I²C-over-AUX, up to three extensions. The AUX engine: request it from the arbiter (`AUX_ARB_CONTROL.AUX_SW_USE_AUX_REG_REQ`, wait for `AUX_REG_RW_CNTL_STATUS` = software), set `AUX_EN` if off, write the message byte by byte into `AUX_SW_DATA` at explicit indices, `AUX_SW_CONTROL` (byte count, `AUX_SW_GO`), poll `AUX_SW_STATUS.AUX_SW_DONE`, timeouts and receive errors from the same register, read the reply back by index, acknowledge `AUX_SW_DONE_ACK`, hand the engine back (`AUX_SW_DONE_USING_AUX_REG`). Receive errors are retried three times.
- HDMI and DVI EDID (the `DC_I2C` engine and the DDC pad mux) is not implemented: those connectors are `Unknown` with only the firmware's mode.

## Not implemented

- **Mode setting.** On AMD hardware the PLL, PHY and encoder sequences are run through the VBIOS's AtomBIOS command tables (`SetPixelClock`, `DIGxEncoderControl`, `UNIPHYTransmitterControl`, `EnableDispPowerGating`, …); DCN also needs clock (DCCG/SMU) and bandwidth (watermark, DET, DLG/TTU) programming. That means an AtomBIOS interpreter (bytecode with its own operand addressing, workspace, IO and data tables), the VBIOS image (from the ACPI `VFCT` table the GOP publishes, or the ROM BAR), and the DML bandwidth model, host-tested against real VBIOS dumps. Not attempted: `amd=modeset` is reserved for it.
- The VBIOS object table (connector types, HPD/DDC/AUX assignments), HDMI EDID, lighting dark connectors, MST, DSC, FreeSync/VRR, PSR, overlays, gamma.
- Restoring the display after sleep: if the firmware lit it again the driver flips back to its buffers and sets the cursor up again; if not, it logs that and the screen stays dark (unchanged from the firmware framebuffer path).
- Interrupts: flips and hot-plug are polled; `Caps::vblank_irq` is false.

## Register details not confirmed

Marked `UNCONFIRMED` in the source:

- The segment bases on DCN 1.0, 3.1.4–3.1.6 and 3.2/3.2.1 (no ip_offset header).
- Field *values*: the DCN pixel format code for ARGB8888 (8); the DCE `GRPH_FORMAT` value 0 for ARGB8888 at 32 bpp; cursor mode 2 = premultiplied ARGB (DCN `CURSOR_MODE`, `CUR0_MODE`, DCE `CURSOR_MODE`); `CURSOR_PITCH` 0 = 64 pixels; `CURSOR_LINES_PER_CHUNK` 3 (8 lines) for a 64-pixel ARGB cursor; `CURSOR_SETTINGS` chunk adjust 3; `AUX_REG_RW_CNTL_STATUS` 1 = software owns the engine; explicit-index access to `AUX_SW_DATA`; sync polarity bit meaning (only the reported mode).
- The timing register convention (counting from the start of sync); it only affects the reported mode.
- The OTG/HPD field positions on DCN 3.0.1, 3.1.5, 3.1.6 and 3.2.0 (missing from those headers; the same as every other version).
- Whether the cursor memory needs powering up (`CURSOR_MEM_PWR_CTRL`) on some DCN versions; the driver doesn't touch it.
- That VRAM above the GOP framebuffer is unused.

Field positions used are otherwise taken from the `*_sh_mask.h` headers and identical on every listed version, except `SURFACE_FLIP_PENDING` (handled per version) and the widths of the cursor mode fields (the values used fit both).

## Testing

- `cargo test -p aurora-amdgfx`: the device table (ids, blocks, unsupported reasons), every layout (all registers present, aligned, distinct, inside 256 KiB; spot checks against the headers), timing encode/decode, DCN takeover (single pipe, split pipes, scaled) and every decline with no writes, DCE 11.2 and DCE 12 takeover and their declines, flips with completion and the frame counter on DCN 1.0, 3.1.4 and DCE 11.2, cursor placement and register programming on DCN and DCE, AUX DPCD and EDID (parsed by `aurora-edid`) on DCN 3.1.4, 3.2 and DCE 11.2 with the engine handed back, HPD sense, VRAM placement.
- `cargo test -p aurora-nvgfx`: Nvidia device ids and `NV_PMC_BOOT_0` decoding.
- `cargo xtask test`: the "amd display" kernel test runs the driver against the simulated engine (a Phoenix laptop, DP panel on AUX 0): takeover, the panel's EDID and kind, the firmware mode, flips into the VRAM stand-in with flip events and the pixels in place, dirty copies, a second buffer and back, refused mode changes and sizes, the cursor's image, position, move and hide, hot-plug of a DP monitor on HPD 3 and its removal, a DCE 11.2 takeover and flip, and declines that write nothing.

## On real hardware: what to send back

1. Boot with no `amd=` option. The desktop should look exactly as before (same resolution), with a hardware pointer.
2. Send `\aurora\boot.log` (or `dmesg`), specifically every line starting `amd:`: the device, ASIC, display block and whether the register bases are confirmed; the apertures; where the firmware framebuffer is and which part of VRAM the driver uses; each lit timing generator (pipes, mode, refresh, pitch, scaled, immediate flips); `hardware cursor: yes/no` or why not; each connector; and `took the display over from the firmware` or `declining: …`.
3. Check: no garbage on screen right after the desktop appears (VRAM the driver chose was not free), no tearing or corruption while dragging windows, the pointer's visibility, colours and hot spot (also at the top and left edges), and plugging a DisplayPort monitor in and out (an `amd: DP-2: Connected` line within a few seconds).
4. If anything looks wrong, try `cursor=software`, then `amd=off`, and report which one fixed it.
5. If it declined, the reason line is the most useful part: a `NotTheGop`-style mismatch on an ASIC with unconfirmed bases points at the register layout; a format or tiling decline gives the register value.
