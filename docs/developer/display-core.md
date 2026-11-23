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

Lumen Server uses `output::Output`: a connector, its CRTC, primary and cursor planes. `Output::open_all` opens every connected connector of every device (the primary device's first), each on a CRTC of its own (chosen by `possible_crtcs`, skipping those taken); `Output::open_connector` opens one (hotplug); `Output::disable` switches one off. The back buffer the compositor draws into *is* the primary plane's framebuffer, so presenting is `dirty` on the damaged rectangles: on virtio-gpu there is no copy in the guest at all. Resolution changes (Settings, "Keep this resolution?", following a host window via `follow_preferred`) are new framebuffers committed with `ALLOW_MODESET`. The pointer goes on the cursor plane when there is one (`cursor=software` in boot.conf turns that off); shapes too large for a 64×64 plane at 2× are drawn into the picture for as long as they are shown.

Timing: the compositor's "time by stage" line counts the present as `flush`, and a `present on <driver> <connector>` line every 10 seconds gives each display's frames, mean and max cost, pixels per frame, and frames it held back while busy.

## Several displays

`gui/screens.rs` (the compositor) and `gui/desktop/displays.rs` (the desktop) build on the outputs; the arrangement math is `lumen::displays` (host-tested: `cargo test -p lumen displays`).

- **One space of points.** Every display is a rectangle in one global coordinate space, in points; the main display (menu bar, Harbor, Shoal, Spotlight, notifications, Shore and Wave Play) is at (0, 0) and is the desktop's own `w`×`h`, so everything that was laid out for "the screen" stays on it. The others touch it or each other along an edge (`displays::arrange`, `snap`): no overlaps, no islands. Damage is in global points; each display paints the part that falls on it with a `Canvas` over its own back buffer, at its own scale, whose `origin` is the display's corner (`Canvas::with_origin`).
- **Per display:** mode, scale (its own choice, else the system's, else 2× from 2560×1440), back buffer, wallpaper, the desktop's reusable pictures (scene, drag backdrop: swapped in while that display is painted), material cache, cursor plane and damage. A client window's surface is drawn at the scale of the display it is on: moving to a display with another scale sends `RESIZE` with the new scale, and until the client catches up its old surface is drawn scaled.
- **Presenting.** With one display the present happens right after painting, as before. With several, each display presents from a task of its own (`Presenter`); a display still taking its last picture is not painted — its damage waits (`deferred`) and is painted in full once it is idle — so a slow or stuck display holds up only itself. Mirroring paints the main display only and copies its new pixels to the others (switched to the main display's resolution when they can, and back afterwards).
- **The pointer** crosses between displays where they touch and stops at the outer edges (`displays::constrain`); the cursor plane of the display it is on shows it, the others hide theirs. Input devices count from (0, 0) at the top left of the rectangle around all displays; `input::warp` keeps relative devices in step when the pointer is stopped. (A QEMU USB tablet is absolute over that whole rectangle, not per window.)
- **Hotplug** (`gpu::hotplug`, a host window resized) and preference changes run `Compositor::reconfigure`: displays that went away are switched off (`Output::disable`), new ones opened in their saved resolution, then everything is laid out again and handed to the desktop (`Desktop::set_displays`). Windows move with their display; a removed display's windows go to the main display remembering where they were (`displays::migrate`), and return when the same display (by identity) comes back, unless moved by hand meanwhile.
- **Preferences.** Each display's place, resolution, scale and wallpaper are one preference, `display_<identity hash>` = `x,y,width,height,scale,wallpaper` (`displays::Record`), plus `display_primary` and `display_mirror`. The identity hashes the EDID's maker, product, serial number and name with the connector name, so a monitor gets its arrangement back on any boot. Settings → Display edits them (drag the displays, or put one beside the main display; main display; mirroring; each display's resolution, scale and wallpaper).
- **For apps:** `desktop::DISPLAYS` (a `DisplayInfo` per display, the main one first), `desktop::WINDOW_DISPLAY` and `desktop::DISPLAY_MODES` (no new system call numbers: requests of the `desktop` call), and the `event::DISPLAY` event when a window changes display or the displays change; `corekit::display`, `aurorakit::App::display_changed` and `waveui::Env::display` wrap them. Programs that don't ask see the main display, as before (`SCREEN`, `sys_info`).

Interfaces added to the core for this (all additive): `Output::open_all`, `Output::open_connector`, `Output::disable`, `Output::framebuffer`, `Output::connector_info`. Nothing in `KmsDevice` or the object model changed.

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
- `cargo xtask test --gpu virtio --displays N` (2–4): the `displays: …` kernel test runs the compositor on N scanouts. The harness plugs the extra heads in and out like a VNC viewer resizing its window (`SetDesktopSize`, 0×0 unplugs; `xtask/src/testdisplays.rs`) and checks what each head shows with QMP `screendump`s against the compositor's back buffers. It covers the layout, a new window opening on the pointer's display, the pointer crossing and stopping at an edge, damage presented only on its display, a display slowed to 300 ms per present not holding up the others, a window leaving an unplugged display and coming back, arranging, changing the main display and mirroring through preferences, the arrangement restored by identity on the second boot, and the frame cost with 3, 2 and 1 displays. With one display (any `--gpu`) it runs the single-display part.
- `tools/vnc.py` (with `AURORA_QEMU_ARGS="-vnc unix:$PWD/target/vnc.sock"`): the cursor image and hot spot the host received, and `resize:W,H` to resize the "host window" as a VNC viewer does.
