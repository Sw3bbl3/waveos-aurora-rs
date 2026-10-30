# Linux programs on WaveOS

WaveOS runs programs built for Linux (x86-64) next to its own. The kernel
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
Japanese and Korean text, and glibc's locale data: about 150 packages,
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
  `/proc/self/exe`, `/proc/cpuinfo`, `/proc/meminfo`, `/proc/mounts`; and
  `/etc/passwd`, `/etc/group`, `/etc/hostname`, `/etc/hosts` and
  `/etc/resolv.conf` built from the computer's settings.
- **Memory:** `brk`, `mmap` (anonymous and private file mappings, hints,
  `MAP_FIXED`), `mprotect`, `mremap`, `munmap` and `madvise`. Anonymous
  memory gets its pages when they are first touched, so a program can
  reserve gigabytes (WebKit's JavaScript engine maps 8 GiB) and pay only
  for what it uses; `MADV_DONTNEED` gives pages back. `PROT_NONE`
  reservations take no memory at all.
- **The processor:** x87, SSE and AVX, with the kernel saving each thread's
  registers (XSAVE) across switches and in signal frames, as on Linux.
- **Shared memory:** `memfd_create`, `shm_open` (`/dev/shm`) and
  `MAP_SHARED` mappings of them, or anonymous ones: every process mapping
  the object sees the same memory, across `fork` and when the descriptor is
  passed over a local socket. `ftruncate` and `fallocate` size them.
- **Threads:** `clone` for threads with thread-local storage (`arch_prctl`),
  `futex`, and the child-tid handshake `pthread_join` uses.
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
  reaches a thread when it returns from a system call; a program that never
  makes one only notices signals that end it.
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
clipboard, notifications, reading preferences, and the uptime clock.

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

What isn't there yet: pop-up menus stay inside their window, drag and drop
between programs, GPU buffers (`linux-dmabuf`: programs draw in software),
input methods, and touch.

## Voyage

**Voyage** is WaveOS's web browser: our own, a small C program
(`linux-programs/voyage`) around WebKitGTK 2.50, Debian's build of WebKit,
the engine of Safari, from the Linux packages on the disk. WebKit brings
JavaScriptCore (with its JIT compilers), CSS, web fonts and media; Voyage
is the browser around it, in GTK 3: a toolbar that is the window's title
bar (back, forward, reload, an address field that also searches, with
DuckDuckGo, and a new tab), tabs, a start page, find in page, zoom,
downloads to WaveOS's Downloads folder, and cookies and site data kept in
`~/.local/share/voyage`. It's in the dock, Shoal and Seek (in Terminal:
`voyage`), and web links and `.html` files open in it.

The Voyage app (`userland/apps/voyage`) runs `/System/Linux/bin/voyage`,
which starts `wl-bridge` and then the browser, with the dark GTK theme when
WaveOS is dark (pages see `prefers-color-scheme: dark`). Without WebKit on
the disk, the app opens Firefox, or else NetSurf. All tabs share one web
process (WebKit's network process is another), since each process starts
WebKit from scratch; in QEMU emulating the processor, Voyage shows a page
in about 15 seconds where Firefox takes about 30.

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

WebKit's sandbox relies on Linux features WaveOS doesn't have yet
(namespaces, seccomp), so the launcher turns it off: browse sites you
trust. Pages draw in software. WebKit's disk cache stores some entries as
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
`linux-tests/linuxtest.c` (files, memory, threads, clocks, `/proc` and
`/dev`, the WaveOS calls) and `linux-tests/proctest.c` (fork, exec, wait,
scripts and the shell, `posix_spawn` and `vfork`, signals and handlers,
interrupted calls, `poll`, `select`, `epoll`, `eventfd`, `timerfd`,
close-on-exec and symbolic links), and `linux-tests/nettest.c` (local
sockets with descriptor passing, TCP and UDP over loopback, name lookup).

To see what a Linux program asks of the kernel, put `WAVEOS_STRACE=1` in
its environment: every system call it makes, with its arguments and
result, goes to the system log.
