# The display core (`kernel/src/drivers/gpu/`)

For engineers writing display or GPU drivers (Intel, AMD, Nvidia, virtio-gpu 3D) and the DRM device node. The code is the reference; this note gives the shape of it and the plan.

## Object model

Every display driver implements `KmsDevice` (`gpu/mod.rs`) and exposes the objects Linux KMS has, with ids (`ObjId`, unique across devices, never 0):

| Object | Type | What it is |
|---|---|---|
| CRTC | `CrtcInfo`, state `CrtcState` | A scanout engine: active or not, a `Mode`, the connectors it drives |
| Plane | `PlaneInfo` (`Primary`, `Cursor`, `Overlay`), state `PlaneState` | Feeds a CRTC from a framebuffer: `src` rectangle in 16.16 fixed point, `dst` on the CRTC, cursor hot spot |
| Encoder | `EncoderInfo` | Links CRTCs (`possible_crtcs` mask) to connectors; bookkeeping for now |
| Connector | `ConnectorInfo` | Kind (HDMI-A, DP, eDP, Virtual…), status, modes (preferred first), raw EDID, size in mm |
| Buffer object | `bo::BufferObject` (`Arc`) | Pixel memory: system RAM in physically contiguous runs, mapped once into the kernel; `chunks()` is the DMA scatter list. A driver binds its own object to it (`bind`, e.g. a virtio resource id) and is told on the last drop (`Unbind`) before the memory is freed |
| Framebuffer | `bo::Framebuffer` (`Arc`) | A buffer object viewed as an image: width, height, pitch, offset, DRM fourcc |
| Mode | `mode::Mode` | Field-for-field DRM's `drm_mode_modeinfo` (timings, clock, flags, type) |

`Caps` says what the device can do: live mode changes, any size (virtual displays), whether `dirty` is needed, cursor plane size, vblank interrupts, whether the preferred mode follows a host window.

EDID is parsed by `libs/edid` (`aurora-edid`, host-tested with `cargo test -p aurora-edid`): base block (identity, size, established, standard and detailed timings, name, range limits), CTA-861 (detailed timings, short video descriptors with a VIC table, HDMI VSDB) and DisplayID type I timings. Standard timings are completed with CVT reduced blanking (`aurora_edid::cvt_rb`); for real panels a DMT table lookup should come first (DMT and CVT differ for some sizes).

## State changes: atomic commits

`KmsDevice::atomic_commit(&Commit)` takes the CRTCs and planes that change, all or nothing, with DRM's flag values (`TEST_ONLY`, `ALLOW_MODESET`, `PAGE_FLIP_EVENT`, `NONBLOCK`). `gpu::validate` merges a request into the committed state and checks the shared rules (objects exist, planes go to CRTCs they can feed, sources inside framebuffers, a primary plane covers its CRTC unscaled, cursor fits the plane, active CRTCs have a mode and a connector, a mode change needs `ALLOW_MODESET`). Drivers call it, then add their own limits in `atomic_check`.

- **Page flips without copies.** Putting a different framebuffer on a primary plane is a flip. On virtio-gpu the buffer's memory is the host resource's backing, so a client buffer can be scanned out directly once it is wrapped in a `Framebuffer`.
- **Damage.** `dirty(fb, rects)` (DRM's DIRTYFB) shows changed rectangles of a framebuffer wherever it is scanned out. Devices that scan out of VRAM (GOP, Bochs) copy the rectangles there; virtio-gpu transfers and flushes them.
- **Events.** `gpu::post(Event::Flip { crtc, seq, time_ns, user_data })` after a commit with `PAGE_FLIP_EVENT`; `Event::Hotplug` from `gpu::hotplug()`. `KmsDevice::vblank(crtc)` is the frame counter and timestamp. Devices without vblank interrupts (virtio-gpu) count a frame when the device has taken the picture. Commits are synchronous today; `NONBLOCK` is accepted and ignored.
- **Sleep.** `resume()` restores the committed state; `gpu::resume_all()` runs from `drivers::resume`.

## Drivers today

| Driver | File | Notes |
|---|---|---|
| Firmware framebuffer | `gop.rs` | One fixed mode; `firmware()` is true, so `gpu::register` retires it when a real driver arrives |
| Bochs/QEMU std VGA | `bochs.rs` | DISPI registers, any listed mode at run time |
| Ported Linux virtio-gpu | `linux.rs` | Only with `gpu=linux` in boot.conf (`cargo xtask run --gpu virtio-linux`) |
| Native virtio-gpu | `virtio/` | Default for virtio GPUs |

The three simple ones share `simple.rs` (one CRTC, one primary plane, one connector; the backend sets modes and shows rectangles).

**virtio-gpu** is split so 3D can reuse the bottom half:

- `virtio/proto.rs`: the wire format, 2D and 3D command codes, the control header (with context id and ring index).
- `virtio/cmd.rs`: `Gpu`, the command layer. A pool of request/response slots on the control queue, `submit` (batchable, optional extra read-only buffer), `wait`, fences (`Cmd::set_fence`, `fence_done`, `wait_fence`), resource ids, the cursor queue, reset after sleep. Nothing display-specific.
- `virtio/mod.rs`: the KMS device. One head per scanout (CRTC, primary and cursor plane, encoder, Virtual connector with EDID). A framebuffer's buffer object becomes a `RESOURCE_CREATE_2D` + `ATTACH_BACKING` resource on first use; `dirty` sends one transfer and one flush per rectangle, notifies once and waits once; the cursor plane uses `UPDATE_CURSOR`/`MOVE_CURSOR`. A `virtio-gpu` task reads `EVENT_DISPLAY` (the MSI-X handler flags it), probes again and reports hotplug.

A virgl/Venus implementation adds context commands (`CTX_CREATE`, `SUBMIT_3D`, blob resources) through `Gpu::submit` with `Header::ctx`, and fences through `wait_fence`; it needs a per-context ring index and asynchronous fence completion (a reaper task woken by the interrupt) rather than the synchronous waits 2D uses.

## The compositor's side

Lumen Server uses `output::Output`: the primary device's first connected connector, its CRTC, primary and cursor planes. The back buffer the compositor draws into *is* the primary plane's framebuffer, so presenting is `dirty` on the damaged rectangles: on virtio-gpu there is no copy in the guest at all. Resolution changes (Settings, "Keep this resolution?", following a host window via `follow_preferred`) are new framebuffers committed with `ALLOW_MODESET`. The pointer goes on the cursor plane when there is one (`cursor=software` in boot.conf turns that off); shapes too large for a 64×64 plane at 2× are drawn into the picture for as long as they are shown.

Timing: the compositor's "time by stage" line counts the present as `flush`, and a `present on <driver>` line every 10 seconds gives frames, mean and max cost and pixels per frame.

## Multi-monitor plan

The core already enumerates every head (QEMU: `AURORA_QEMU_ARGS="-global virtio-vga.max_outputs=2"` gives Virtual-1 connected and Virtual-2 disconnected; QEMU enables the second output only when a UI shows it, e.g. a GTK window or a second VNC display bound to that console), and each CRTC can be committed independently. The desktop still uses one `Output`. To extend:

1. `Output::open_all()`: one output per connected connector, each with its own CRTC (choose CRTCs by `possible_crtcs`, avoiding conflicts) and back buffer.
2. A layout in desktop coordinates (positions per output, saved in preferences), with the desktop's screen becoming the union rectangle; damage is clipped per output and each output presents its share.
3. The pointer moves between outputs: the cursor plane of the output under it shows it, the others hide theirs.
4. Hotplug adds or removes outputs and moves windows off a vanished one.
5. Different scales per output need the desktop to paint per output; start with one scale.

## Where the DRM ioctl layer attaches

A `/dev/dri/card0` (and `renderD128`) node in the Linux compatibility layer (`kernel/src/compat/linux`) maps almost one to one:

| DRM ioctl | Core call |
|---|---|
| `MODE_GETRESOURCES`, `GETCRTC`, `GETPLANERESOURCES`, `GETPLANE`, `GETENCODER`, `GETCONNECTOR` | `crtcs()`, `planes()`, `encoders()`, `probe()`/`connectors()`, `state()` |
| `MODE_OBJ_GETPROPERTIES`, `GETPROPERTY`, `GETPROPBLOB` | to add: a property table derived from the infos and state (plane `type`, `FB_ID`, `CRTC_ID`, `SRC_*`, `CRTC_*`; CRTC `ACTIVE`, `MODE_ID`; connector `EDID`, `CRTC_ID`, `DPMS`) |
| `MODE_ATOMIC` | build a `Commit` from the property writes; flags pass through |
| `MODE_SETCRTC`, `PAGE_FLIP`, `CURSOR`/`CURSOR2`, `SETPLANE` | the same, as small commits |
| `MODE_CREATE_DUMB`, `MAP_DUMB`, `DESTROY_DUMB` | `create_dumb`; map the buffer object's frames into the process (they are ordinary frames) |
| `MODE_ADDFB2`, `RMFB` | `Framebuffer::new` on a handle's buffer object |
| `MODE_DIRTYFB` | `dirty` |
| `GEM_CLOSE`, `PRIME_HANDLE_TO_FD`, `PRIME_FD_TO_HANDLE` | a per-file handle table of `Arc<BufferObject>`; a dma-buf fd holds another `Arc` |
| `WAIT_VBLANK`, `CRTC_GET_SEQUENCE`, events on `read()` | `vblank()`; `gpu::take_events()` becomes a per-file queue |
| `SYNCOBJ_*` | to add, on top of fences (`Gpu::wait_fence` on virtio) |

When clients drive KMS directly, the compositor should become one of them (or hand the device over, DRM-master style); until then a client commit and the compositor's would race for the same CRTC.

## Testing

- `cargo test -p aurora-edid`: EDID base block, CTA-861, DisplayID, bad checksums, CVT.
- `cargo xtask test --gpu std|virtio|virtio-linux`: the `display: …` kernel test (connectors and EDID, two mode sets, a full and a partial present, an atomic rule, a page flip with its event, the cursor plane) and, in the sleep test, a present after waking.
- `tools/vnc.py` (with `AURORA_QEMU_ARGS="-vnc unix:$PWD/target/vnc.sock"`): the cursor image and hot spot the host received, and `resize:W,H` to resize the "host window" as a VNC viewer does.
