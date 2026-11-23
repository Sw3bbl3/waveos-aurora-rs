# The display core (`kernel/src/drivers/gpu/`)

For engineers writing display or GPU drivers (Intel, AMD, Nvidia, virtio-gpu 3D) and working on the DRM device node. The code is the reference; this note gives the shape of it and the plan.

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

Its commits and presents go through `gpu::owner` (`desktop_commit`, `desktop_dirty`), which keeps them aside while a Linux program drives the display (see [Who drives the display](#who-drives-the-display)); anything else that shows the desktop should do the same.

Timing: the compositor's "time by stage" line counts the present as `flush`, and a `present on <driver>` line every 10 seconds gives frames, mean and max cost and pixels per frame.

## Multi-monitor plan

The core already enumerates every head (QEMU: `AURORA_QEMU_ARGS="-global virtio-vga.max_outputs=2"` gives Virtual-1 connected and Virtual-2 disconnected; QEMU enables the second output only when a UI shows it, e.g. a GTK window or a second VNC display bound to that console), and each CRTC can be committed independently. The desktop still uses one `Output`. To extend:

1. `Output::open_all()`: one output per connected connector, each with its own CRTC (choose CRTCs by `possible_crtcs`, avoiding conflicts) and back buffer.
2. A layout in desktop coordinates (positions per output, saved in preferences), with the desktop's screen becoming the union rectangle; damage is clipped per output and each output presents its share.
3. The pointer moves between outputs: the cursor plane of the output under it shows it, the others hide theirs.
4. Hotplug adds or removes outputs and moves windows off a vanished one.
5. Different scales per output need the desktop to paint per output; start with one scale.

## The DRM device (`/dev/dri/card0`)

Linux programs reach the display core through the DRM uAPI, in the Linux compatibility layer (`kernel/src/compat/linux/drm/`): Linux's ioctl numbers and struct layouts, copied in and out by the size encoded in each ioctl number and zero-extended (so programs built against older or newer headers work), and Linux's error codes. `linux-tests/drmtest.c` writes the structs out by hand and checks their sizes against Linux's.

**Nodes and enumeration.** `card<N>` is `gpu::devices()[N]` (char 226:N). `/sys/class/drm/card0` and `/sys/dev/char/226:0` link to `/sys/devices/pci0000:00/<slot>/drm/card0` (`dev`, a `uevent` with `DEVNAME=dri/card0`, connector folders with `status`, `modes`, `edid`…); the PCI device has `vendor`, `device`, `subsystem_*`, `revision`, `class`, `config` and a `uevent` with `PCI_SLOT_NAME`, and its `subsystem` links to `/sys/bus/pci` (`compat/linux/sysfs.rs`). That is everything libdrm's `drmGetDevices2`, `drmGetDevice2`, `drmGetDeviceNameFromFd2` and `drmGetMinorNameForFD` read. Every card is presented as a PCI device (the firmware framebuffer as the boot VGA's), because libdrm cannot enumerate a platform device without device-tree properties.

**Driver identity.** `DRM_IOCTL_VERSION` gives the name Linux's driver for the same hardware has — `virtio_gpu`, `bochs-drm`, `simpledrm` — since Mesa picks its user-space driver by it (with no 3D, Mesa's virgl fails to start and GBM falls back to `kms_swrast`). A hardware driver implements `drm::Driver`: name, description, date, version, `render()` (a render node `renderD<128+N>` is offered when true) and `ioctl()` for its private range `0x40`–`0x9F` (argument copied in and out like the core ones). Core ioctls not allowed on render nodes are refused there with `EACCES`.

| Group | ioctls | Notes |
|---|---|---|
| Core | `VERSION`, `GET_UNIQUE`, `SET_VERSION`, `GET_CAP`, `SET_CLIENT_CAP`, `GET_MAGIC`, `AUTH_MAGIC`, `SET_MASTER`, `DROP_MASTER`, `MODESET_CTL` | `GET_UNIQUE` is empty until a master asks for interface 1.1, then `pci:0000:00:02.0` (libdrm's `drmOpenByName` depends on that). Caps: dumb buffers, PRIME import+export, monotonic timestamps, CRTC in vblank events, cursor size; no modifiers, syncobjs, async flips or flip targets. Client caps: stereo, universal planes, atomic (implies universal planes), aspect ratio, writeback and cursor hotspot (both need atomic). |
| Objects | `GETRESOURCES`, `GETCRTC`, `GETENCODER`, `GETCONNECTOR`, `GETPLANERESOURCES`, `GETPLANE` | Ids are the core's. `GETCONNECTOR` with no room for modes probes again. Planes other than overlays need universal planes. |
| Properties | `OBJ_GETPROPERTIES`, `GETPROPERTY`, `GETPROPBLOB`, `CREATEPROPBLOB`, `DESTROYPROPBLOB`, `OBJ_SETPROPERTY`, `SETPROPERTY` | Plane `type`, `FB_ID`, `CRTC_ID`, `SRC_*`, `CRTC_*`, `HOTSPOT_X/Y` (cursor planes, with the hotspot cap); CRTC `ACTIVE`, `MODE_ID`; connector `EDID`, `DPMS`, `CRTC_ID`, `link-status`, `non-desktop`. Atomic-only ones are listed to atomic clients only. Mode and EDID blobs are the kernel's (deduplicated, the oldest of more than 64 dropped). |
| Changes | `ATOMIC`, `SETCRTC`, `SETPLANE`, `PAGE_FLIP`, `CURSOR`, `CURSOR2`, `DIRTYFB` | Each becomes a core `Commit` built on the current state (`props::Request`), checked by the core and the driver. `ATOMIC`: `TEST_ONLY`, `NONBLOCK`, `ALLOW_MODESET`, `PAGE_FLIP_EVENT` (one event per CRTC the commit touches); no async flips. A blocking commit returns once its frame is on screen; a nonblocking one, and `PAGE_FLIP`, fail with `EBUSY` while the CRTC's previous flip is pending. |
| Buffers | `CREATE_DUMB`, `MAP_DUMB`, `DESTROY_DUMB`, `ADDFB`, `ADDFB2`, `RMFB`, `CLOSEFB`, `GETFB`, `GETFB2`, `GEM_CLOSE` | A file's handle table holds `Arc<BufferObject>`s (32-bit dumb buffers come from the device's `create_dumb`). `MAP_DUMB` gives `buffer id << 32` as the `mmap` offset; the mapping holds the buffer's frames (`BufferObject::user_frames` counts them like shared memory), so they outlive the handle and the buffer. Formats are what some plane takes (XRGB8888, ARGB8888); no modifiers. `RMFB` of a framebuffer on screen turns its planes (and, for a primary, the CRTC) off, as Linux does. |
| PRIME | `PRIME_HANDLE_TO_FD`, `PRIME_FD_TO_HANDLE` | A dma-buf is a descriptor holding the buffer object: it passes over `SCM_RIGHTS`, maps with `mmap` from offset 0, tells its size with `lseek(SEEK_END)`, takes `DMA_BUF_IOCTL_SYNC`; importing gives the handle the buffer already has in that file. |
| Vblank | `WAIT_VBLANK`, `CRTC_GET_SEQUENCE`, `CRTC_QUEUE_SEQUENCE` | See below. |

**Vblank and events.** Devices with vblank interrupts count frames themselves (`KmsDevice::vblank`, `Caps::vblank_irq`). The others get a steady clock per CRTC at its mode's refresh rate (frame *n* begins *n* periods after boot), and a flip completes at the next frame: a program flipping on each event runs at the refresh rate, as on Linux. Events (`FLIP_COMPLETE`, `VBLANK`, `CRTC_SEQUENCE`; at most 4 KiB per file) wait in the file until due; a timer wakes `poll`, `epoll` and blocking `read`s then, and `read` returns whole events only. The DRM layer makes the flip events itself after the (synchronous) commit and does not pass `PAGE_FLIP_EVENT` to drivers; a driver whose commits become asynchronous must report completion to it instead.

### Who drives the display

The first program to open a card while nobody is master becomes master, as on Linux; `SET_MASTER` takes master when it is free (WaveOS has no seat manager: the person at the computer owns its display, so no privilege is needed) and `DROP_MASTER` gives it up. Opening the card, reading state, making buffers and testing commits take nothing from the desktop. The master's first real change — a mode set, plane, cursor, page flip or atomic commit — **takes the display** (`gpu::owner::client_commit`):

- The desktop's state (every CRTC and plane) is kept aside, and its cursor and overlay planes are turned off in the same commit.
- While the program holds the device, `Output`'s commits go through `owner::desktop_commit` and change only the kept state (checked by the core's rules all the same), and its presents (`owner::desktop_dirty`) reach nothing. Lumen Server goes on drawing into its back buffer.
- When the master drops master, or its file closes — the program exited, crashed or was killed, since a process's descriptors are closed when it is cleaned up — `owner::give_back` commits the kept state again with `ALLOW_MODESET` (from a blank state if that fails) and shows the desktop's framebuffers in full. The desktop is back without the compositor doing anything.

Input still goes to the desktop while a program holds the display (there is no evdev yet), and nothing but the program's end gives the display back; a key that revokes master (as a VT switch does on Linux) is a follow-up. With several programs, master passes between them without the desktop in between: the kept state stays the desktop's.

### What Mesa still needs

Mesa's software path over KMS is `kms_swrast` (llvmpipe or softpipe drawing into dumb buffers, shown with `ADDFB2` and page flips), reached through GBM's DRI backend or EGL on GBM. What it uses is here: driver name and PCI identity, dumb buffers with `MAP_DUMB` and `mmap`, PRIME export and import, `ADDFB2`, page flips and atomic commits with events, `GET_CAP`. libdrm itself is exercised by `modetest` (below). Not verified, and still missing:

- **Running it.** Debian's Mesa 22.3 (`libgbm1`, `libgl1-mesa-dri`) is in the Linux packages but no test has loaded it against the card yet. llvmpipe needs executable memory from `mmap`/`mprotect` (present) and enough address space for LLVM.
- **Driver selection.** On virtio-gpu Mesa first tries virgl, which asks `DRM_IOCTL_VIRTGPU_GETPARAM` (driver-private: `EINVAL` here) and gives up; GBM then falls back to `kms_swrast` (`GBM_ALWAYS_SOFTWARE=1` or `LIBGL_ALWAYS_SOFTWARE=1` skips the attempt). On `bochs-drm` and `simpledrm` no hardware driver exists and the fallback is immediate.
- **Render nodes.** None are offered (no driver renders). EGL device enumeration (`EGL_EXT_device_drm_render_node`) and headless clients that open `renderD128` find nothing; GBM on `card0` works without one.
- **Formats and modifiers.** Only XRGB8888/ARGB8888 and linear buffers: no `IN_FORMATS` blob, `ADDFB2_MODIFIERS` is 0, so `gbm_bo_create_with_modifiers` falls back to the plain call. RGB565, 10-bit and XBGR formats are refused.
- **Synchronisation.** No syncobjs, sync files (`DMA_BUF_IOCTL_EXPORT_SYNC_FILE`/`IMPORT_SYNC_FILE` say `ENOTTY`), `IN_FENCE_FD` or `OUT_FENCE_PTR`. Software rendering finishes before it is shown, so implicit sync is enough for it; hardware drivers need syncobjs on top of their fences.
- **Legacy sharing.** No `GEM_FLINK`/`GEM_OPEN` (DRI2 and X servers only), no DRM leases, no gamma/CTM, no writeback connectors.
- **Input.** A full-screen KMS program (kmscube, a game) needs evdev for keys and the pointer.

## Testing

- `cargo test -p aurora-edid`: EDID base block, CTA-861, DisplayID, bad checksums, CVT.
- `cargo xtask test --gpu std|virtio|virtio-linux`: the `display: …` kernel test (connectors and EDID, two mode sets, a full and a partial present, an atomic rule, a page flip with its event, the cursor plane) and, in the sleep test, a present after waking.
- `cargo xtask test --gpu std|virtio`, the `drmtest` kernel test: `linux-tests/drmtest.c` (static, musl) enumerates the card the way libdrm does, checks the core ioctls and master rules, objects and properties, makes dumb buffers and draws through `mmap`, sets a mode, flips four times with events (through epoll), makes atomic commits (a failing test, a mode change refused without `ALLOW_MODESET` and done with it, a nonblocking flip with its event), waits for vblanks, moves and hides the cursor plane (or sees `ENXIO` without one), exports a dma-buf, maps it, passes it to another process over a socket and imports it there, checks error paths, and checks that the desktop comes back when it closes the card and when a child holding the display is killed. While it shows red, green, blue and white quadrants, and again after the kill, it writes `harness: screendump NAME` to `/dev/kmsg`; `xtask` takes a QEMU screendump (QMP) and checks the quadrants and the stand-in desktop's colour (`check_screendumps`).
- `cargo xtask test`, the `modetest` kernel test: libdrm 2.4.114's own `modetest`, built unmodified with the library, static with musl (`xtask/src/libdrm.rs`, into `/System/Linux/bin/modetest`), opens the card by driver name (`-M`), lists everything, sets a mode and page-flips on every event for 6 seconds (`-s … -v`, ending when its input does), then runs its atomic loop (`-a -s … -P … -v`, blocking commits at the refresh rate) until it is killed, after which the desktop must have the display back. It reports about 60 flips per second on every device.
- `tools/vnc.py` (with `AURORA_QEMU_ARGS="-vnc unix:$PWD/target/vnc.sock"`): the cursor image and hot spot the host received, and `resize:W,H` to resize the "host window" as a VNC viewer does.
