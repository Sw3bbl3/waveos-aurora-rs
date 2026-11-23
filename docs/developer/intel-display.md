# Intel display driver (`kernel/src/drivers/gpu/intel`, `libs/igfx`)

For engineers working on the Intel integrated graphics display driver. It is a display core driver (see [display-core.md](display-core.md)); rendering (the GT, execbuffer, Mesa) is not part of it.

**Status: written from Intel's public Programmer's Reference Manuals and checked against a simulated display engine; it has not run on real hardware.** The default behaviour is designed so that the worst outcome on a machine it misjudges is the firmware framebuffer path WaveOS had before: every probe step is read-only until the driver has decided to take over, and anything unexpected declines.

## Layout

| Where | What |
|---|---|
| `libs/igfx` (`aurora-igfx`, `no_std`, host-tested) | Everything that is register programming over the `Mmio` trait: device table, register names, timing packing, firmware readback, plane values, DDB and watermark arithmetic, GGTT allocator, GMBUS, DP AUX, DP link selection and training, Skylake DPLL dividers, PWM backlight, Skylake-class mode set sequences, and `mock`, a simulated display engine |
| `kernel/src/drivers/gpu/intel/mod.rs` | The `KmsDevice`: probe, GGTT binding of buffer objects, flips, cursor, damage flushing, connectors and hotplug, backlight, `intel=` policy |
| `kernel/src/drivers/gpu/intel/hw.rs` | BAR 0 as `Mmio` + `gtt::Table` (registers in the first 2 MiB, the GGTT from 8 MiB) |
| `kernel/src/drivers/gpu/intel/selftest.rs` | The driver against `aurora_igfx::mock` in `cargo xtask test` |

The DisplayPort side of AUX (DEFER retries, DPCD, I²C-over-AUX, EDID) is the `aux::Transport` trait's, over any controller that moves one message; the AMD driver (`libs/amdgfx`) implements it for its AUX engine, and the simulated sink's replies (`mock::Sink::handle`) are shared the same way.

The kernel driver is generic over `Hw: Mmio + gtt::Table`, so the self-test runs the real takeover, GGTT, flip, cursor and hotplug code against the simulated engine.

## Coverage

| Display version | Platforms | Takeover, flips, cursor, EDID, backlight | Mode changes (`intel=modeset`) |
|---|---|---|---|
| 9 | Skylake, Kaby Lake (R, Amber Lake), Coffee Lake (Whiskey Lake), Comet Lake | yes | Skylake-class: yes, within the limits below |
| 9 | Apollo Lake (Broxton) | yes | no (different PHY and PLLs) |
| 10 | Gemini Lake | yes | no |
| 11 | Ice Lake, Elkhart Lake, Jasper Lake | yes | no (combo/Type-C PHYs, different PLLs) |
| 12 | Tiger Lake, Rocket Lake, Alder Lake-S, Raptor Lake-S | yes | no |
| 13 | Alder Lake-P, Alder Lake-N, Raptor Lake-P/U | yes | no |

Device ids are in `libs/igfx/src/platform.rs`. Broadwell and older, Meteor Lake and newer (display 14+: AUX and PHYs moved) and discrete GPUs are not listed and are left to the firmware framebuffer, with a log line saying why.

## Boot options

| `boot.conf` | Effect |
|---|---|
| (nothing) or `intel=takeover` | Keep the firmware's mode; scan out WaveOS's buffers, hardware cursor, EDID, hotplug, backlight |
| `intel=modeset` | Also allow resolution changes (Skylake-class only; see below) |
| `intel=off` | Don't bind: the firmware framebuffer (GOP) as before |
| `cursor=software` | (existing) also leaves the display buffer allocation alone: no cursor plane |

## Probe and takeover (`takeover::read`, `Intel::new`)

1. PCI class 0x03, vendor 0x8086; the device id must be in the table. BAR 0 must be a 16 MiB memory BAR. `GGC` (config 0x50) gives the GGTT size (`GGMS`, 7:6) and stolen memory size (`GMS`, 15:8); `BDSM` (0x5C, or 0xC0 on display 11+) and `ASLS` (0xFC, the OpRegion) are logged.
2. Read-only readback, per transcoder (`TRANSCODER_EDP` first on display 9–11, then A–D):
   - `TRANSCONF` (`PIPECONF`) enable, interlace mode;
   - `TRANS_DDI_FUNC_CTL`: enable, DDI select (30:28, or 30:27 as index + 1 on display 12+), mode (HDMI, DVI, DP SST), bits per colour, sync polarity, eDP input select, DP width;
   - `HTOTAL`, `HBLANK`, `HSYNC`, `VTOTAL`, `VBLANK`, `VSYNC`, and the pipe's `PIPESRC`;
   - plane 1: `PLANE_CTL` (enable, format, RGB order, tiling, rotation), `PLANE_STRIDE`, `PLANE_POS`, `PLANE_SIZE`, `PLANE_SURF`, `PLANE_BUF_CFG`, `PLANE_WM_0..7` (and `PLANE_WM_SAGV` on display 12+), `CURCNTR`;
   - `PP_STATUS` (panel on: DDI A in DP mode is eDP), `CDCLK_CTL` (display 9–10).
3. Declines (`takeover::Decline`, logged as `intel: declining: …`): no running pipe; two transcoders on one pipe; interlaced; MST or an unknown DDI mode; nonsensical timings; wider than 5120 (joined pipes); plane 1 off, tiled, not 32-bit RGB, rotated or mirrored, not covering the pipe, or with a stride shorter than a row.
4. Pixel clock: read back on Skylake-class (`DPLL_CTRL1`/`DPLL_CTRL2`/`DPLL_CFGCR1/2` for HDMI; link rate code and `PIPE_LINK_M1/N1` for DP), else the EDID's timing with the same totals, else measured from `PIPE_FRMCOUNT` over six frames, else assumed 60 Hz.
5. GGTT: the driver allocates above the firmware's mappings (the larger of stolen memory, the end of every firmware framebuffer, and the last in-use entry found by sampling one entry per MiB), from a 16 MiB boundary. If fewer than 64 MiB remain, it declines.

Only after that does it write anything: `DE_PIPE_IMR` (unmask vblank and plane 1 flip done so `DE_PIPE_IIR` latches them; `IER` is untouched and no MSI is set up, so no interrupt is ever raised), `SDEIMR` (so hotplug latches in `SDEIIR`), the cursor's display buffer (below), and GGTT entries in its own range.

## Scanout

- Framebuffers are system RAM (`BufferObject`), mapped into the GGTT on first use (`Ggtt::bind`: one 64-bit PTE per page, `phys | 1`, then `GFX_FLSH_CNTL`), 256 KiB aligned (the display 9 rule for linear surfaces; later versions need less), followed by 16 guard entries pointing at a scratch page because the display engine prefetches past the end. When the buffer object is freed the range is pointed at the scratch page and returned.
- A flip writes `PLANE_CTL` (the firmware's value with the surface fields replaced: 8:8:8:8 BGRX, linear, no rotation), `PLANE_STRIDE` (64-byte units), `PLANE_OFFSET` = 0 and `PLANE_SURF` last (it arms the others for the next vblank), then waits up to 100 ms for `PLANE_SURFLIVE` to show the new address. The flip event's sequence number is `PIPE_FRMCOUNT`.
- Watermarks and the display buffer allocation are the firmware's: same bytes per pixel, tiling and width, so they still hold.
- When the firmware lit several displays (a laptop with a monitor attached at boot, showing one framebuffer), a pipe no client has given a picture of its own scans the same buffer as the one that was flipped, if the sizes match; otherwise it keeps the firmware's last picture. Buffers are released only after every pipe scanning them has flipped away.
- GGTT entries the firmware points at one scratch page of its own (the most common page among the sampled entries) count as free.
- The display engine doesn't snoop CPU caches: `dirty` flushes the damaged rows with `clflush`; a new framebuffer is flushed whole before it is flipped to. Framebuffers whose pitch isn't a multiple of 64 bytes (1366 wide, for one) get a padded shadow buffer that `dirty` copies into.
- `create_dumb` tries one physically contiguous run for screen-sized buffers, so `panic_framebuffer` can hand the panic screen the scanout buffer; the panic screen ends with `wbinvd` for the same cache reason.

## Cursor plane (`ddb::plan_cursor`)

The GOP gives plane 1 the whole pipe's display buffer and the cursor none. The driver takes 8 blocks (more if the watermark needs it) off the end of plane 1's `PLANE_BUF_CFG`, disables plane 1's watermark levels whose blocks no longer fit (and `PLANE_WM_TRANS` if needed), arms that with a `PLANE_SURF` write and waits a vblank; then writes `CUR_BUF_CFG`, `CUR_WM_0` (plane 1's level-0 lines, one block per 64-pixel ARGB line plus margin), levels 1–7 and `CUR_WM_TRANS` off, `CUR_WM_SAGV` on display 12+, and arms with `CURBASE`. A level is only entered when every plane has it enabled, so the cursor's missing levels cost some power, not correctness. The cursor is 64×64 ARGB (`CURCNTR` mode 0x27), premultiplied from the compositor's straight alpha into one of two images (alternated, so an update never tears), `CURPOS` in sign-magnitude, `CURBASE` written last.

## Connectors, EDID, hotplug

- One connector per DDI: those the firmware lit, plus DDI B–D (display 9), B–C (Broxton/Gemini Lake) or B (display 11+). Type-C ports are only listed when the firmware lit them (their PHYs need an ownership handshake).
- DP is detected by a DPCD read over AUX (`DP_AUX_CH_CTL` + five data registers, 32 sync pulses, the longest timeout, DEFER and receive-error retries); EDID then comes over I²C-over-AUX (MOT, 16-byte reads, address-only stop, segment pointer at 0x30). HDMI/DVI EDID comes over GMBUS (`GMBUS0`–`GMBUS5`, 100 kHz, indexed reads). Up to three extension blocks are read.
- The `intel` task reads `SDEIIR`, `DE_PORT_IIR` and (display 11+) `DE_HPD_IIR` every 500 ms; anything latched triggers a re-probe, and every 4 s it re-probes anyway (the bits differ between PCHs and aren't all confirmed). A change posts `Event::Hotplug`.
- Lit connectors list their current mode (and, with `intel=modeset`, the EDID modes the mode set code accepts); others list their EDID modes, but can't be lit yet. Lit connectors come first, so `Output::open` picks one of them.

## Backlight

With an eDP panel lit and the south display PWM enabled (`BLC_PWM_CTL1` bit 31), the driver keeps the firmware's period and changes the duty: `BLC_PWM_PCH_CTL2` 15:0 on Sunrise Point (Skylake/Kaby Lake), `BXT_BLC_PWM_DUTY1` elsewhere; polarity (bit 29) inverts. Never below 5 %. It is registered with `gpu::set_backlight`, and the brightness keys change it by 10 %. Panels driven over DPCD (many OLED/HDR panels) are not handled.

## Mode changes (`intel=modeset`, `aurora_igfx::modeset`)

Skylake-class only, on a pipe the firmware lit, keeping its port and output type, following the Skylake PRM's DisplayPort and HDMI enable/disable sequences:

- **Disable**: backlight off (T9), cursor and plane off, `TRANSCONF` off (wait for state), `TRANS_DDI_FUNC_CTL` off, `DDI_BUF_CTL` off (wait idle), `DP_TP_CTL` off, `DPLL_CTRL2` clock gate, the DPLL off unless shared (never DPLL 0, which runs the CD clock).
- **Enable**: `DPLL_CTRL1` (HDMI mode, or the DP link rate code), `DPLL_CFGCR1/2` for HDMI from `dpll::hdmi` (the PRM's DCO algorithm: 24 MHz reference, AFE = 5 × pixel clock, DCO within +1 %/−6 % of 8400/9000/9600 MHz, even dividers preferred), wait for `DPLL_STATUS` lock; route the port's clock in `DPLL_CTRL2`; eDP panel power with the sequencer's delays; for DP: `SET_POWER` D0, `DP_TP_CTL` pattern 1, `DDI_BUF_CTL` on, link rate (`LINK_BW_SET`, or `LINK_RATE_SET` with an eDP 1.4 rate table) and lane count, clock recovery and channel equalization (`dp::train`, drive levels mapped to `DDI_BUF_TRANS` entries 0–9), idle pattern; timings, `PIPESRC`, `PLANE_SIZE`, DP data/link M/N (N = 0x8000, TU 64), `TRANS_DDI_FUNC_CTL`, `TRANSCONF` (wait for state), normal pattern, backlight (T8).
- **Limits**: only modes no larger than the firmware's in pixel clock, line rate and width (the firmware's display buffer and watermarks then still suffice; larger modes need the PCU's memory latencies and a watermark computation); no CD clock changes; eDP keeps its timings; the firmware's buffer translations are reused (not programmed); no power wells touched. A port the firmware didn't light can't be lit.
- **Failure**: `modeset::switch` disables the new configuration and enables the firmware's again; the commit fails and the compositor keeps its framebuffer.

## Not implemented

- Lighting ports the firmware left dark (needs the VBT from the OpRegion for DDC pins, AUX channels, buffer translations; `DDI_BUF_TRANS` tables; power wells).
- Watermark computation (PCU memory latency mailbox), CD clock changes, pipe scaler programming, larger modes.
- Display 11+ PLLs and PHYs (combo PHY, Type-C/Thunderbolt), MST, DSC, PSR, FBC, HDR, audio.
- Restoring the display after sleep: the display core (power wells, CD clock, DBUF) is off after S3 and the firmware doesn't re-initialize it; the driver logs this. (Unchanged from the firmware framebuffer path.) If the firmware did light the pipes again, the driver gives the cursor its display buffer share again and flips back to its buffers.
- Real interrupts: completion is polled from `PLANE_SURFLIVE`/`PIPE_FRMCOUNT` and the identity registers, so `Caps::vblank_irq` is false.
- VBT parsing: GMBUS pins and AUX channels are Intel's reference defaults (marked UNCONFIRMED in `platform.rs`).

## Register details not confirmed

Marked `UNCONFIRMED` in the source: `DP_TP_STATUS` idle-done bit; `BDSM` at 0xC0 on display 11+; GMBUS pin numbers per PCH; AUX channel per port; the AUX "sync pulse" counts (32, the Skylake value) and timeout field on later platforms. Also not checked against hardware: the watermark field widths on display 12–13 (lines 26:14, blocks 11:0), `PLANE_WM_SAGV`/`CUR_WM_SAGV` offsets, whether `DE_PIPE_IIR` latches with `IER` clear on every platform (flip completion does not depend on it), and the Kaby Lake R / Amber Lake PCH pairing (Sunrise Point assumed).

## Testing

- `cargo test -p aurora-igfx`: device table (ids unique, platforms and display versions), timing register values for CEA 1080p, DPLL dividers for 148.5, 25.175, 297 and 594 MHz against values worked out by hand from the PRM algorithm (and every clock 25–300 MHz inside the DCO range), DP link selection, M/N, eDP rate tables, drive levels, link training against a scripted sink (success, clock recovery and channel EQ failures), AUX framing, AUX and GMBUS against the simulated engine with EDID parsed by `aurora-edid`, takeover readback on Skylake eDP and Tiger Lake HDMI and every decline, pixel clock readback, plane values and cursor encoding, cursor DDB plans, GGTT space and `GGC` decoding, backlight, HDMI and DP mode switches with link training, and a failed switch that restores the firmware mode.
- `cargo xtask test`: the "intel display" kernel test runs the driver against the simulated engine (Kaby Lake laptop): takeover, connectors and EDID, the clock from the EDID, the cursor's DDB carve, flips into GGTT-mapped RAM with flip events, GGTT release, a refused mode change, cursor image/move/hide, backlight, hotplug, a 1366-wide padded scanout, and declines that write nothing.

## On real hardware: what to send back

1. Boot with no `intel=` option. The desktop should look exactly as before (same resolution), with a hardware cursor.
2. Send `\aurora\boot.log` (or `dmesg`), specifically every line starting `intel:`. They give the device id and platform, `GGC`, stolen memory and GGTT layout, each running pipe (transcoder, output, DDI, mode, clock, lanes, bpc), the cursor's DDB carve or why there is none, the backlight PWM, each connector with its EDID, and either `took the display over from the firmware` or `declining: …`.
3. Check: no flicker or corruption (especially at the cursor and while moving windows: that would be a watermark or cache-flush problem), the pointer shape and hot spot, brightness keys on a laptop, plugging and unplugging an external monitor (a `HDMI-A-1: Connected` line should appear within a few seconds).
4. If anything looks wrong, try `cursor=software` (no cursor plane, display buffer untouched), then `intel=off`, and report which one fixed it.
5. Optional, Skylake/Kaby/Coffee/Comet Lake with an external monitor: `intel=modeset`, then Settings → Display, pick a smaller resolution. Report the `intel: pipe …` / `mode set … failed` lines.
