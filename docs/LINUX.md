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
coreutils, Python 3, the DejaVu fonts and glibc's locale data: about 140
packages, 440 MiB).
They come from a fixed snapshot of the Debian archive, and every download
is checked: the archive's `Release` file against a SHA-256 pinned in
`xtask/src/debian.rs`, the package list against `Release`, and each package
against the list. Symbolic links are recorded in `.links`; maintainer
scripts don't run. What their triggers would generate for GTK programs (the
MIME database and compiled GSettings schemas) is made with the build
machine's `update-mime-database` and `glib-compile-schemas`
(`shared-mime-info` and `libglib2.0-bin` on Debian and Ubuntu; `brew install
shared-mime-info glib` on macOS).

`cargo xtask run --linux-root` makes a fresh disk with that root as
`/Linux`. The system folders are layered: what the disk has (`/Linux/usr`,
`/Linux/lib`, …) comes first, then the system image (`/System/Linux`), so a
Debian program takes the place of the BusyBox command of the same name.
In the shell, `bash`, `python3` and the rest run as on Debian.

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
- **Made-up files:** `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/fd/*`;
  `/proc/self/exe`, `/proc/cpuinfo`, `/proc/meminfo`, `/proc/mounts`; and
  `/etc/passwd`, `/etc/group`, `/etc/hostname`, `/etc/hosts` and
  `/etc/resolv.conf` built from the computer's settings.
- **Memory:** `brk`, `mmap` (anonymous and private file mappings, hints,
  `MAP_FIXED`), `mprotect`, `mremap`, `munmap` and `madvise`. `PROT_NONE`
  reservations take no memory until made accessible.
- **Shared memory:** `memfd_create`, `shm_open` (`/dev/shm`) and
  `MAP_SHARED` mappings of them, or anonymous ones: every process mapping
  the object sees the same memory, across `fork` and when the descriptor is
  passed over a local socket. `ftruncate` and `fallocate` size them.
- **Threads:** `clone` for threads with thread-local storage (`arch_prctl`),
  `futex`, and the child-tid handshake `pthread_join` uses.
- **Processes:** `fork` copies the program's memory; `vfork` and
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
  namespace, `socketpair`, `SO_PEERCRED`, and descriptors passed with
  `SCM_RIGHTS`. `sendmmsg`/`recvmmsg` and `FIONREAD` too.
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

## Firefox ESR

With the Linux packages on the disk (`cargo xtask run --linux-root`),
**Firefox ESR** is in Shoal and Seek (in Terminal: `firefox`). It runs
Debian's Firefox ESR 140, unchanged, through `/System/Linux/bin/firefox`:
that script starts `wl-bridge --background` (which returns once the bridge
is listening, or at once if it already is) and then Firefox with Wayland
on. Firefox runs in several processes as usual (content, sockets, media),
loads and renders pages over HTTP and HTTPS with its own root
certificates, and takes the keyboard, mouse, scrolling and clipboard.
Without the packages, the app says how to get them.

Firefox's sandboxes rely on Linux features WaveOS doesn't have yet
(seccomp filters, namespaces), so the launcher turns them off and Firefox
shows a notice that some security features offer less protection. It
draws in software. Starting takes a while in QEMU without hardware
acceleration (libxul is read in full when it loads).

## NetSurf

The web browser, **NetSurf**, is a Linux program too: NetSurf 3.11 built
static with musl, with a libnsfb surface that is a WaveOS window, in
`/System/NetSurf`. It needs nothing on the disk. See [NETSURF.md](NETSURF.md).

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
