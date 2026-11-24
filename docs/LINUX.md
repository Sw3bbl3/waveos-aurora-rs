# Linux programs on WaveOS

WaveOS runs programs built for Linux (x86-64, and arm64 on the aarch64
port) next to its own. The kernel
recognises them when it starts them, gives them the start-up Linux programs
expect, and carries out their system calls with Linux's numbers and meanings.
The code is in [`kernel/src/compat/linux`](../kernel/src/compat/linux).

This is being built in steps. What works today:

| Step | What it brings | State |
|---|---|---|
| M0 | Static programs: files, memory, threads, clocks | Done |
| M1 | `fork`, `exec`, `wait`, signals, `poll`/`epoll`: an interactive shell | Done |
| M2 | Linux programs open WaveOS windows and play sound; Doom | Done |
| M3 | Dynamically linked programs (glibc), Debian packages | Done |
| M4 | Sockets (TCP, UDP, local) | Done |
| M5 | Shared memory and a Wayland bridge, for GTK programs | Done |
| M6 | Firefox ESR | Done |
| A1 | arm64 programs: the generic call table, arm64 layouts, signal frames, vDSO | Done: the static tests and Debian's hello and curl pass under HVF |

## Two ABIs: x86-64 and arm64

On the aarch64 port, Linux programs are arm64 ones (Debian's `arm64`
packages, `aarch64-unknown-linux-musl` builds). The layer implements
each call once; what differs between the ABIs is in
`kernel/src/compat/linux/abi/` and `libs/linuxabi` (details in
[developer/porting.md](developer/porting.md#the-linux-layers-abis)):

| | x86-64 | arm64 |
|---|---|---|
| System call | `syscall`; number in rax, arguments rdi, rsi, rdx, r10, r8, r9 | `svc #0`; number in x8, arguments x0-x5, result in x0 |
| Call table | Linux's x86-64 table (the layer's numbering) | the generic table: no `open`, `stat`, `fork`, `pipe`, `poll`, `select`, `dup2`, `rename`, `mkdir`, `epoll_wait`… only their `*at`/`p*`/`*2`/`3` forms, which x86-64 programs use too |
| `clone` | `(flags, stack, ptid, ctid, tls)` | `(flags, stack, ptid, tls, ctid)` |
| TLS | FS base, `arch_prctl` | TPIDR_EL0, set by the program or `clone(CLONE_SETTLS)` |
| `struct stat` | 144 bytes | 128 bytes (32-bit `st_nlink`, `st_blksize`) |
| `struct epoll_event` | packed, 12 bytes | 16 bytes |
| Open flags | `O_DIRECTORY` 0o200000, `O_NOFOLLOW` 0o400000, `O_DIRECT` 0o40000, `O_LARGEFILE` 0o100000 | 0o40000, 0o100000, 0o200000, 0o400000 |
| `MINSIGSTKSZ` | 2048 | 5120 |
| Signal frame | SIMD state (XSAVE), restorer address, `ucontext`, `siginfo` | frame record, `siginfo`, `ucontext` with `fpsimd_context` (and `esr_context` for faults); x30 = the vDSO's `__kernel_rt_sigreturn` unless `SA_RESTORER` |
| Restarted calls | rip -= 2, rax = the number | pc -= 4, x0 = the first argument |
| vDSO | `__vdso_clock_gettime`, `__vdso_gettimeofday`, `__vdso_time` (TSC) | `__kernel_clock_gettime`, `__kernel_gettimeofday`, `__kernel_clock_getres`, `__kernel_rt_sigreturn` (CNTVCT_EL0) |
| Auxiliary vector | `AT_HWCAP` = `cpuid` 1 EDX, `AT_PLATFORM` "x86_64" | `AT_HWCAP`, `AT_HWCAP2` from the ID registers, `AT_PLATFORM` "aarch64", `AT_MINSIGSTKSZ` |
| `/proc/cpuinfo` | `flags`, family/model | `Features`, `CPU implementer`/`part`… |
| `uname -m` | `x86_64` | `aarch64` |

Everything else they read and write — `flock`, `statfs`, `termios`,
`siginfo`, `sigaction`, signal numbers, `mmap`/`clone`/`SA_` flags, ioctl
numbers, errno — is the same, and the host tests (`cargo test -p
aurora-linuxabi`) check each value against Debian 12's Linux and glibc
headers for both architectures (`cargo xtask linux-abi --arch <arch>`
regenerates them). arm64's `PROT_BTI` and `PROT_MTE` are accepted and
ignored; BTI, MTE, SVE and SME are not offered to programs.

Not yet on arm64: `extra_context` frames (only needed with SVE), the
DRM device's test on `virt` (its display is ramfb, not PCI), Mesa and the
browsers (next). musl's `vfork` is plain `fork` on arm64 (it has no
`vfork` of its own there).

## The shell

In Terminal, `sh -i` starts BusyBox's shell interactively: pipelines,
loops, scripts, `cd`, `trap` and the rest of `ash`. Ctrl+C sends SIGINT to
the program and the commands it runs (a second Ctrl+C ends them). There is
no terminal device yet, so the shell runs without job control and
full-screen programs (`vi`, `less`) don't work.

## Which programs are Linux programs

WaveOS's own programs carry a `WaveOS` ELF note (`corekit` adds it). A
program without the note runs as a Linux program, except in the folders
that only ever held WaveOS programs (`/Applications`, `/System/Apps`,
`/System/Bin`, `/System/Developer`); there, a program without the note is
still a Linux program if it has a GNU note, an interpreter or the Linux
OS/ABI byte.

Dynamically linked programs start in their interpreter (`ld.so`), which
the kernel loads next to them; it then loads the libraries itself.

## Debian packages

`cargo xtask linux-root [PKG…]` fetches Debian 12 packages and everything
they depend on into `target/linux-root` (by default Firefox ESR, bash,
coreutils, Python 3, the DejaVu fonts, WenQuanYi Micro Hei for Chinese,
Japanese and Korean text, glibc's locale data and Mesa's `kmscube`: about 150 packages,
450 MiB). Without arguments it also rebuilds a root that lacks one of these.
They come from a fixed snapshot of the Debian archive, and every download
is checked: the archive's `Release` file against a SHA-256 pinned in
`xtask/src/debian.rs`, the package list against `Release`, and each package
against the list. Symbolic links are recorded in `.links`; maintainer
scripts don't run. What their triggers would generate for GTK programs (the
MIME database and compiled GSettings schemas) is made with the build
machine's `update-mime-database` and `glib-compile-schemas`
(`shared-mime-info` and `libglib2.0-bin` on Debian and Ubuntu; `brew install
shared-mime-info glib` on macOS). A disk that gets new packages gets these
files again too. Without that tool the schemas are missing, so Voyage
compiles its own copy the first time it starts (WebKit's network process
needs them).

`cargo xtask run` puts that root on the disk as `/Linux`, fetching the
packages the first time: a new disk is made large enough for them, and an
older disk gets the ones it lacks; when its volume has no room for them,
xtask makes the disk bigger first, copying every file on it to the bigger
one and checking the copy before it takes the old disk's place. `--no-linux-root` leaves them off, and
when they can't be fetched (offline) WaveOS runs without them. `cargo
xtask image` does the same for the USB image. WaveOS's own settings for the
programs (Firefox's preferences and policies, GTK's settings; see
`linux-programs/firefox`) are written into the root on every build and
onto the disk on every run; when the root's packages change (its
`packages.txt`), the files the disk doesn't have yet are added too. The system folders are layered: what the disk has (`/Linux/usr`,
`/Linux/lib`, …) comes first, then the system image (`/System/Linux`), so a
Debian program takes the place of the BusyBox command of the same name.
In the shell, `bash`, `python3` and the rest run as on Debian.

Libraries and other files programs map (`mmap`) are read once and shared
(`kernel/src/fs/mapcache.rs`): every process mapping a file gets the same
pages, read-only, and a process that makes one writable gets its own copy.
Writing to the file, or deleting it, drops it from the cache.

## What a Linux program sees

- **Start-up:** `argc`, `argv`, the environment and the auxiliary vector
  (`AT_PHDR`, `AT_ENTRY`, `AT_RANDOM`, `AT_EXECFN`, `AT_HWCAP`…) on an 8 MiB
  stack. The environment has `PATH`, `HOME=/home/<you>`, `USER`, `SHELL`,
  `TERM`, `LANG=C.UTF-8`, `XDG_RUNTIME_DIR=/run` and
  `WAYLAND_DISPLAY=wayland-0`.
- **Files:** a Linux-shaped tree. `/bin`, `/usr`, `/lib`, `/etc`, `/opt`
  and `/sbin` are the read-only `/System/Linux`; `/home`, `/tmp`, `/var`,
  `/root`, `/run`, `/srv`, `/mnt` and `/media` are the writable `/Linux`.
  WaveOS folders (`/Documents`, `/Downloads`, `/System`…) are where they
  always are.
- **Made-up files:** `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/fd/*`,
  `/dev/kmsg` (the system log: `2>/dev/kmsg` keeps a program's messages);
  the input devices and what describes them (`/dev/input`, `/dev/hidraw*`,
  `/dev/uinput`, `/sys`, `/run/udev`; see below);
  `/proc/self/exe`, `/proc/self/auxv`, `/proc/meminfo` (with swap and the compressed store
  as `Zswap`), `/proc/pressure/memory`, `/proc/self/status` and
  `/proc/<pid>/status` (`VmRSS`, `VmSwap`, `VmLck`), `/proc/mounts`;
  `/proc/cpuinfo` and `/sys/devices/system/cpu`, as Linux writes them
  from `cpuid` (the `flags` line lists what the processor has and the
  kernel enables);
  `/dev/dri/card0` (see [The display](#the-display-drm)), the parts of
  `/sys` that describe it (`/sys/class/drm`, `/sys/dev/char`, the PCI
  device) and its udev record (`/run/udev/data/c226:0`); and
  `/etc/passwd`, `/etc/group`, `/etc/hostname`, `/etc/hosts` and
  `/etc/resolv.conf` built from the computer's settings.
- **Memory:** `brk`, `mmap` (anonymous and private file mappings, hints,
  `MAP_FIXED`), `mprotect`, `mremap`, `munmap` and `madvise`. Anonymous
  memory gets its pages when they are first touched, so a program can
  reserve gigabytes (WebKit's JavaScript engine maps 8 GiB) and pay only
  for what it uses; `MADV_DONTNEED` and `MADV_FREE` give pages back,
  `MADV_PAGEOUT` swaps them out now, `MADV_COLD` makes them the first to
  go, `MADV_WILLNEED` brings swapped-out ones back. `PROT_NONE`
  reservations take no memory at all. `mlock`, `mlock2`, `munlock`,
  `mlockall` and `munlockall` keep pages in memory (`MCL_FUTURE` is
  accepted, but memory mapped later isn't locked); `sysinfo` reports swap.
  When memory runs short, pages are swapped out (see
  [developer/memory.md](developer/memory.md)).
  `mincore` tells whether pages are mapped and in memory (a swapped-out
  page is not).
- **The processor:** x87, SSE and AVX, with the kernel saving each thread's
  registers (XSAVE) across switches and in signal frames, as on Linux.
- **Shared memory:** `memfd_create`, `shm_open` (`/dev/shm`) and
  `MAP_SHARED` mappings of them, or anonymous ones: every process mapping
  the object sees the same memory, across `fork` and when the descriptor is
  passed over a local socket. `ftruncate` and `fallocate` size them.
- **Threads:** `clone` for threads with thread-local storage (`arch_prctl`),
  and the child-tid handshake `pthread_join` uses. `futex` waits and wakes
  (with deadlines and bit sets), moves waiters from one futex to another
  (`FUTEX_REQUEUE`, `FUTEX_CMP_REQUEUE`, which `pthread_cond_broadcast`
  uses so it wakes one thread instead of all) and does `FUTEX_WAKE_OP`. A
  futex in shared memory without `FUTEX_PRIVATE_FLAG` is the same futex
  in every process that maps it.
- **Time:** every program gets a vDSO (`AT_SYSINFO_EHDR`, built from
  `kernel/vdso/vdso.c`): `clock_gettime`, `gettimeofday` and `time` read
  the processor's time-stamp counter and a page the kernel keeps up to
  date, without a system call, and give the same time the kernel would.
  Where the TSC isn't the clock they ask the kernel.
- **Processes:** `fork` shares the program's memory copy-on-write; `vfork` and
  `posix_spawn` run the child in the parent's memory until it calls `exec`.
  `execve` runs Linux programs and `#!` scripts in place (descriptors marked
  close-on-exec close); `exec` of a WaveOS program runs it as a process of
  its own and passes its exit code on. `wait4` and `waitid`, process groups
  and sessions (`setpgid`, `setsid`) work as on Linux.
- **Signals:** `rt_sigaction` handlers run with a real signal frame
  (`siginfo`, the registers and SIMD state in a `ucontext`, `sigaltstack`)
  and return with `rt_sigreturn`; masks, `kill`, `tgkill`, `sigsuspend`,
  `sigtimedwait`, `SIGCHLD` when a child ends and `SIGPIPE` on a closed pipe.
  Blocking calls return `EINTR`, or start again with `SA_RESTART`. A signal
  reaches a thread when it returns from a system call, or at once if it is
  busy in its own code: the sender interrupts the processor running it.
  (JavaScript engines stop their threads this way, for garbage collection
  among other things.) Faults become signals a program can handle: a bad
  memory access is `SIGSEGV` (`SEGV_MAPERR` or `SEGV_ACCERR`, with the
  address), an invalid instruction `SIGILL`, a division by zero `SIGFPE`,
  a misaligned access `SIGBUS`, `int3` `SIGTRAP`. The handler gets every
  register in its `ucontext` and may change them before it returns. A
  program without a handler crashes as before, with the report in the
  system log.
- **Waiting:** `poll`, `ppoll`, `select`, `pselect6`, `epoll`, `eventfd` and
  `timerfd` (all level-triggered), woken as soon as a pipe, event, timer or
  the network changes.
- **Sockets:** TCP and UDP over IPv4 and IPv6 on WaveOS's own network
  stack (blocking and non-blocking, `poll`/`epoll` readiness, `SO_ERROR`
  after a non-blocking `connect`, timeouts), so name lookups through
  `/etc/resolv.conf` and HTTP work. Local sockets (`AF_UNIX`): stream,
  datagram and sequenced-packet, named by a path or in the abstract
  namespace, `socketpair`, `SO_PEERCRED`, descriptors passed with
  `SCM_RIGHTS`, and the sender's credentials with `SO_PASSCRED`
  (`SCM_CREDENTIALS`). `sendmmsg`/`recvmmsg` and `FIONREAD` too.
- **Symbolic links:** the read-only tree's links come with the system image
  (`/bin/sh` and every other BusyBox command are links to `/bin/busybox`,
  where Debian puts them); `symlink` makes new ones in the writable tree.
  `O_PATH | O_NOFOLLOW` opens a link itself, and `readlinkat(fd, "")`
  reads it (systemd's libraries walk paths this way). `statfs` says
  sysfs, procfs and tmpfs for `/sys`, `/proc`, `/dev` and `/run`.
- **inotify:** folders are watched for files and folders created,
  deleted and renamed (and device nodes in `/dev`); changes to a file's
  contents aren't reported yet.
- **Everything else:** calls that aren't there yet return `ENOSYS`, and the
  first use of each is written to the system log (`linux: … system call N
  is not supported yet`).
- **`/proc/self/maps`** lists the program's memory (glibc finds the main
  thread's stack there).

## WaveOS windows and sound from Linux programs

Linux has no windows of its own, so Linux programs reach WaveOS's windows,
events, sound streams, clipboard and notifications through call numbers from
`0x5741_0000` up: `0x5741_0000 + n` is WaveOS's call `n`, with WaveOS's
arguments. [`libs/linux/include/waveos.h`](../libs/linux/include/waveos.h)
wraps them for C:

```c
#include <waveos.h>

long w = wave_win_create(640, 400, "Hello", 5, WAVE_WIN_RESIZABLE);
struct wave_surface s;
wave_win_surface(w, &s);           /* s.addr: 0xAARRGGBB pixels, s.stride per row */
/* … draw … */
wave_win_present(w, 0, 0, s.width, s.height);
struct wave_event e;
while (wave_next_event(&e, 16) == 1) { /* keys, pointer, resize, close */ }
```

Only these WaveOS calls are open to Linux programs: windows, events, sound
(`wave_audio_open`, then `write` 48 kHz stereo 16-bit samples), the
clipboard, notifications, reading preferences, the uptime clock, and game
controllers (`wave_gamepads`, `wave_gamepad_state`, `wave_gamepad_rumble`:
one standard layout for every controller; Doom uses them). A program reads
a neutral controller while it isn't the one in front. Programs written for
Linux don't need these for input: see below.

## Keyboards, mice and game controllers the Linux way

Linux programs find and read input devices as on Linux, so SDL, libinput,
Steam's input layer and Wine work unchanged
([developer/linux-input.md](developer/linux-input.md) has the details):

- `/dev/input/event*` for every keyboard, mouse, tablet, touchpad and game
  controller, with Linux's events, `ioctl`s, force feedback (rumble) and
  Caps Lock light; `/dev/input/js*` (the old joystick API) for
  controllers; `/dev/hidraw*` for USB HID devices (raw reports, feature
  reports); `/dev/uinput` to make devices (they work in WaveOS too).
- Controllers look exactly as Linux's drivers show them (Xbox pads as
  `xpad`, PlayStation as `hid-playstation`, Switch Pro as `hid-nintendo`),
  so SDL's built-in controller database knows them. The keyboard-played
  controller (Super+G) is an Xbox 360 pad.
- Finding them: sysfs (`/sys/class/input`, `/sys/class/hidraw`, the devices
  under `/sys/devices`), `/dev/input/by-id` and `by-path`, udev's records in
  `/run/udev/data`, device events over netlink for libudev's monitors (the
  kernel sends udev's messages itself), and inotify on `/dev/input`.
- Debian's SDL 2.26 finds the keyboard-played controller, plugged in
  while it runs, and calls it a game controller with its built-in Xbox 360
  mapping, through libudev and through inotify alike (`sdlprobe`, see the
  developer page).
- Who reads what: keyboards, mice and touchpads reach only the program
  whose window has the focus; controllers, the program in front (Wave
  Play's rule). A program without the focus sees its devices at rest. The
  device files belong to the account logged in at the screen; raw HID
  files of keyboards and mice belong to the system.

Three calls fit windows into a program's own main loop:

- `wave_event_fd(O_CLOEXEC)` gives a descriptor that `poll` and `epoll`
  report readable while the program has events waiting; `read` gives how
  many, and `wave_next_event(e, 0)` takes them.
- After the compositor has put a window's present on screen, the window
  gets a `WAVE_EV_FRAME` event (one at a time), so a program can draw its
  next frame at the screen's pace instead of guessing.
- `wave_win_set_cursor(window, shape)` sets the pointer over the window:
  the arrow, a text I-beam, a pointing hand, a crosshair, busy, not
  allowed, resize left–right or up–down, move, or none.

Programs written for Linux's own sound interfaces reach the same streams
through OSS: `/dev/dsp` is a WaveOS sound stream, and its `ioctl`s (format,
channels, rate, buffer space, delay, sync, reset) describe it as it is,
48 kHz 16-bit stereo, so a program that asks for another rate is told the
one it gets and converts (GStreamer's `osssink` does, and with it WebKit's
media). There is no PulseAudio or ALSA.

## The display (DRM)

Each display is a DRM card, `/dev/dri/card0`, with Linux's ioctls (libdrm
and anything built on it work as they are): the mode-setting objects and
their properties, mode sets, planes, the cursor, page flips and atomic
commits with their events (read from the card, through `poll` or `epoll`),
vblank waits, dumb buffers mapped through the card, and PRIME: a buffer as
a dma-buf descriptor that can be mapped and passed to another program over
a local socket. `drmGetDevices2` finds the card through `/sys`, as on Linux.
The driver names are Linux's (`virtio_gpu`, `bochs-drm`, `simpledrm`).
libdrm's test program is included: `modetest -M virtio_gpu` (or
`bochs-drm`) lists the card, and `modetest -M virtio_gpu -s 9:1280x800 -v`
sets a mode and page-flips until you press Return.

The first program to open a card is its master, as on Linux when no
display server runs. Looking costs nothing; its first mode set or page
flip takes the screen from the desktop, which comes back as soon as the
program drops master, closes the card or exits — also when it crashes or
is killed. While it holds the screen, keys and the pointer still go to the
desktop. Mesa runs on it unmodified (see [Mesa](#mesa-opengl-and-vulkan));
render nodes, syncobjs and tiled formats are what hardware drivers will
need next (see
[display-core.md](developer/display-core.md#what-hardware-drivers-still-need)).

## Mesa (OpenGL and Vulkan)

Debian's Mesa 22.3 draws in software on the processor, with llvmpipe
(LLVM compiles shaders to the processor's own code): OpenGL ES 3.2 and
OpenGL 4.5 through EGL, and Vulkan 1.3 through lavapipe.

- **Full screen**, as on a Linux console: GBM and EGL on `/dev/dri/card0`
  (Mesa's `kms_swrast`: llvmpipe draws into dumb buffers, page-flipped
  with events). `kmscube` and `glmark2-drm`/`glmark2-es2-drm` run as they
  are; the program takes the display from the desktop and gives it back
  when it ends.
- **In a window**, through `wl-bridge`: Mesa's EGL on Wayland draws into
  shared memory, and the bridge copies each frame into the WaveOS window
  (`wl-bridge &` then `es2gears_wayland`).
- **Vulkan** with lavapipe: rendering and reading back (`vulkaninfo`
  lists it). Presenting full screen through `VK_KHR_display` is untried.

`kmscube` is among the default packages (in Terminal: `kmscube`; Return
ends it). The others (`glmark2-drm`, `mesa-vulkan-drivers`,
`vulkan-tools`, `mesa-utils-bin`) aren't; `cargo xtask test --mesa` puts
them on its own test disk with `vkcheck`, a small
Vulkan program (`linux-programs/vkcheck`) that xtask builds with clang
against Debian's Vulkan headers, as it builds Voyage. In QEMU emulating
the processor, kmscube's cube at 1280 × 800 runs at 25 to 35 frames per
second; see
[display-core.md](developer/display-core.md#mesa-performance) for where
the time goes.

## Wayland programs (GTK and others)

Programs that draw with GTK, Qt, SDL or their own Wayland code show their
windows through `wl-bridge`, a Wayland compositor in `/System/Linux/bin`
([`linux-programs/wl-bridge`](../linux-programs/wl-bridge), Rust, built for
`x86_64-unknown-linux-musl`). Start it once, then the programs:

```sh
wl-bridge &
gtk3-demo
```

Each top-level window becomes a WaveOS window. The bridge copies every
frame the program commits (shared-memory buffers, `wl_shm`) into the
window, with its sub-surfaces and pop-up menus, and sends WaveOS's pointer,
scroll and key events back as Wayland events (a US keymap, built by the
program's xkbcommon from the system's XKB data). WaveOS draws the window
frame and resizes windows; the program is told the new size and draws
again. Copying in a program offers the text to WaveOS's clipboard, and
WaveOS's clipboard is what programs paste.

OpenGL programs work the same way: with no GPU buffers offered (no
`linux-dmabuf`), Mesa's EGL draws in software into shared memory, and the
bridge copies each frame.

What isn't there yet: pop-up menus stay inside their window, drag and drop
between programs, GPU buffers (`linux-dmabuf`: programs draw in software;
see [display-core.md](developer/display-core.md#mesa-in-a-window) for the
design), input methods, and touch.

## Voyage

**Voyage** is WaveOS's web browser: our own, a small C program
(`linux-programs/voyage`) around WebKitGTK 2.50, Debian's build of WebKit,
the engine of Safari, from the Linux packages on the disk. WebKit brings
JavaScriptCore (with its JIT compilers), CSS, web fonts and media; Voyage
is the browser around it, in GTK 3: a toolbar below the reserved system
window strip (back, forward, reload/stop, an address field that also searches, with
DuckDuckGo, and a new tab), tabs, a start page, find in page, zoom,
downloads to WaveOS's Downloads folder, and cookies and site data kept in
`~/.local/share/voyage`. It's in the dock, Shoal and Seek (in Terminal:
`voyage`), and web links and `.html` files open in it.

The Voyage app (`userland/apps/voyage`) runs `/System/Linux/bin/voyage`,
which starts `wl-bridge` and then the browser, with the dark GTK theme when
WaveOS is dark (pages see `prefers-color-scheme: dark`). Without WebKit on
the disk, the app opens Firefox, or else NetSurf. All tabs share one web
process (WebKit's network process is another), since each process starts
WebKit from scratch. Startup and site compatibility depend on the page,
the host, and whether QEMU is emulating the CPU; do not interpret load
timings as physical-hardware performance.

`cargo xtask` builds Voyage with clang and the Rust toolchain's lld
against the libraries in the Linux packages (`xtask/src/voyage.rs`):
`linux-programs/voyage/webkit.h` declares the parts of GLib, GTK and
WebKitGTK it calls, so only Debian's C library headers and start files
(`libc6-dev`, `linux-libc-dev`) are needed, not those libraries'
development packages. The program goes onto the disk with WaveOS's
settings on every run, so a disk always has the current build. HTTPS is
checked against Mozilla's roots (the `ca-certificates` package, gathered
into `/etc/ssl/certs/ca-certificates.crt` on every run, with the root
`WAVEOS_EXTRA_CA` names, if any).

Navigation errors provide escaped explanations and Retry/Back. TLS uses
WebKit's fail policy; a certificate error cannot silently become an
unverified connection. Escape or the Stop button cancels a pending load.
Closing a tab asks WebKit to apply its before-unload protection. Downloads
show progress, Cancel, completion and failure, and select a unique filename
without allowing overwrite. Local absolute paths are converted to file URIs.

`cargo xtask web-check --output target/network-web/check` prepares a full
disposable guest with WebKit, Firefox, schemas and certificate roots. It
checks real browser interaction against local HTTP/HTTPS fixtures, then a
public site sample. Its generated test CA is installed only in that guest;
normal builds do not trust it. Use `--local-only`, `--net e1000|e1000e|none`,
`--firefox`, or `--repetitions N` as needed. `--benchmark-only` compares
new browser processes and same-process revisits without clearing disk
caches. `--baseline-image PATH` clones an older image and preserves its
kernel/bridge while updating the browser instrumentation. Never point it
at a personal disk for validation.

WebKit's sandbox relies on Linux features WaveOS doesn't have yet
(namespaces, seccomp), so the launcher turns it off: browse sites you
trust. Pages draw in software: Skia paints them on the processor, and
WebKit composites them with Mesa's software OpenGL (llvmpipe). Video and
sound go through GStreamer, with the codecs of `gstreamer1.0-plugins-good`,
`-bad` and `-libav` (VP8, VP9, AV1, H.264, Opus, AAC, so streaming sites'
Media Source Extensions work), and sound plays through `/dev/dsp` (the
launcher points GStreamer at `osssink`). In QEMU emulating the processor,
`cargo xtask run` presents it as a Haswell: QEMU's own `max` model reports
an AMD family 6 processor, which LLVM takes for a 32-bit Athlon XP, and
llvmpipe then can't generate code. WebKit's disk cache stores some entries as
hard links, which WaveOS's file systems don't have, so fewer pages come
from the cache.

## Firefox

**Firefox** is Debian's Firefox ESR 140, unchanged, from the Linux
packages on the disk. It's in Shoal and Seek (in Terminal: `firefox`). The
Firefox app (`userland/apps/firefox`) runs `/System/Linux/bin/firefox`
with the addresses or files it was given, which starts `wl-bridge
--background` (it returns once the bridge is listening, or at once if it
already is) and then Firefox with Wayland on. When WaveOS is dark, Firefox
gets the dark GTK theme, and pages that offer a dark look use it. Without
the packages on the disk, the app opens NetSurf instead.

Firefox runs pages in a few content processes and loads them over HTTP
and HTTPS with its own root certificates; the keyboard, mouse, scrolling,
the clipboard and a mouse's back and forward buttons work. WaveOS's
settings (`linux-programs/firefox`) give it a clean start and a quiet
window:

- no welcome tour, "what's new" page, default-browser question or
  restore-session page after an unclean stop;
- no window buttons of its own (WaveOS's window handle has them; GTK's
  `gtk-decoration-layout` is empty for every GTK program), a compact
  toolbar without the account and Pocket buttons, and a new tab page with
  only a search box (no site tiles or sponsored content);
- nothing sent to Mozilla (telemetry, studies), and downloads in WaveOS's
  Downloads folder;
- fewer processes: pages share up to four content processes instead of one
  per site (site isolation is off), and networking and extensions run in
  processes that are there anyway, since each process starts Firefox from
  scratch.

These are defaults: what you change in Firefox's settings wins.

Media decoding currently runs in the content process: the remote RDD/utility
decoder configuration stalls before its first frame on the compatibility layer,
even when YouTube has buffered data. `media.rdd-process.enabled` and
`media.utility-process.enabled` default to false, with
`media.allow-audio-non-utility` true. This trades decoder crash isolation for
working playback; codecs and playback quality remain available. See the
[Firefox validation report](validation/firefox-performance.md) for experiments,
measurements and the limits of QEMU verification.

Firefox's sandboxes rely on Linux features WaveOS doesn't have yet
(seccomp filters, namespaces), so the launcher turns them off; with site
isolation off too, a page that broke into its content process could read
other sites' pages there. Firefox draws in software. In QEMU emulating
the processor it takes 25 to 31 seconds to show its new tab page (at
1920 × 1080, on four virtual processors); `cargo xtask run` uses the
host's virtualization instead when it has some (KVM, HVF, WHPX), which is
many times faster.

What made it start faster, for anyone working on the layer: the kernel
heap gives small blocks from per-size free lists; `fork` shares pages
copy-on-write (`mm::vmm`, resolved in the page fault handler); `poll`,
`select` and `epoll` wait on keys of the objects they watch
(`proc::readiness`) instead of waking on every change in the system;
wait subscriptions remember matching notifications before checking readiness,
and disabled one-shot epoll timers do not force repeated rescans;
checking user pointers, the signal check on return and finding the
current process take no lock shared by the process's threads; and
`link` fails with `EPERM`, as on file systems without hard links, so that
fontconfig can save its cache. Sampling the virtual processors through
QEMU's monitor (`info registers -a`, then following frame pointers) found
each of these.

## NetSurf

**NetSurf** is the small browser, in the system image: NetSurf 3.11 built
static with musl, with a libnsfb surface that is a WaveOS window, in
`/System/NetSurf`. It needs nothing on the disk and starts in seconds, but
doesn't run JavaScript. It's in Shoal and Seek, and stands in for Firefox
when the Linux packages aren't on the disk. See [NETSURF.md](NETSURF.md).

## Doom

**Doom** in Shoal (under Play) starts the game: [doomgeneric][dg] (GPL-2.0),
built static with musl and a WaveOS backend
([`third_party/doomgeneric/waveos`](../third_party/doomgeneric/waveos)), runs
as a Linux program from `/System/Games/Doom`. The window scales the picture
to any size, sound effects play through WaveOS's mixer, and settings and
saved games go in `~/.doom` (`/Linux/home/<you>/.doom`). Music isn't played.

The game needs game data (a WAD file). The Doom app looks in
**Documents → Doom** for `doom.wad`, `doom1.wad`, `doom2.wad`, `plutonia.wad`,
`tnt.wad`, `freedoom1.wad`, `freedoom2.wad` and `freedm.wad`, and offers to
download [Freedoom][fd] 0.13.0 (BSD-3-Clause, 24 MB, both phases) from its
GitHub release when there is none. The data isn't in the system image
because it would not fit on the installation disc.

Controls: arrow keys move, Ctrl fires, Space opens doors, Shift runs, Alt
strafes, number keys switch weapons, Esc opens the menu.

[dg]: https://github.com/ozkl/doomgeneric
[fd]: https://freedoom.github.io/

## Building Linux programs into the image

`cargo xtask build` builds every `linux-tests/*.c` and Doom static with
musl, and downloads BusyBox from Debian, pinned by SHA-256. C programs are
built with `musl-gcc` where it is installed (Debian/Ubuntu: `sudo apt
install musl-tools`); elsewhere, macOS included, with `clang`, Debian's
`musl-dev` package (downloaded and checked the same way) and the Rust
toolchain's `rust-lld` (`WAVEOS_LINUX_CC=clang` picks this way everywhere).
Without either, the image is built without them (with a warning), and the
kernel's Linux self-tests are skipped. `wl-bridge` is built with Cargo for
`x86_64-unknown-linux-musl` and linked with `rust-lld` too (set in
`.cargo/config.toml`), so it needs nothing besides the Rust toolchain.
Debian packages are unpacked by `xtask` itself; no Debian tools are needed.

Two self-tests run inside WaveOS as part of `cargo xtask test`:
`linux-tests/linuxtest.c` (files, memory, threads, clocks and the vDSO,
futexes, `/proc` and `/dev`, the WaveOS calls, the window event descriptor
and `FRAME`) and `linux-tests/proctest.c` (fork, exec, wait, scripts and
the shell, `posix_spawn` and `vfork`, signals and handlers, faults handled
as signals, signals to a thread busy in its own code,
interrupted calls, `poll`, `select`, `epoll`, `eventfd`, `timerfd`,
close-on-exec and symbolic links), `linux-tests/nettest.c` (local
sockets with descriptor passing, TCP and UDP over loopback, name lookup),
and `linux-tests/inputtest.c` (input devices: evdev, the joystick API,
hidraw, uinput, sysfs and udev records, hot-plug over netlink and inotify,
focus, grabs and rumble, with the kernel test playing the user).

To see what a Linux program asks of the kernel, put `WAVEOS_STRACE=1` in
its environment: every system call it makes, with its arguments, the path
it names (if any) and its result, goes to the system log.
`WAVEOS_SYSCOUNT=1` is lighter: when the program ends (or `exec`s
another), the log gets how many of each call it made and the time they
took, waits included, most first.
