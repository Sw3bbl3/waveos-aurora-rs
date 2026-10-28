# Linux programs on WaveOS

WaveOS runs programs built for Linux (x86-64) next to its own. The kernel
recognises them when it starts them, gives them the start-up Linux programs
expect, and carries out their system calls with Linux's numbers and meanings.
The code is in [`kernel/src/compat/linux`](../kernel/src/compat/linux).

This is being built in steps. What works today:

| Step | What it brings | State |
|---|---|---|
| M0 | Static programs: files, memory, threads, clocks | Done |
| M2 | Linux programs open WaveOS windows and play sound; Doom | Done |
| M1 | `fork`, `exec`, `wait`, signals, `poll`/`epoll`: an interactive shell | Next |
| M3 | Dynamically linked programs (glibc), Debian packages | Planned |
| M4 | Sockets (TCP, UDP, local) | Planned |
| M5 | Shared memory and a Wayland bridge, for GTK programs | Planned |
| M6 | Firefox ESR | Planned |

## Which programs are Linux programs

WaveOS's own programs carry a `WaveOS` ELF note (`corekit` adds it). A
program without the note runs as a Linux program, except in the folders
that only ever held WaveOS programs (`/Applications`, `/System/Apps`,
`/System/Bin`, `/System/Developer`); there, a program without the note is
still a Linux program if it has a GNU note, an interpreter or the Linux
OS/ABI byte.

Dynamically linked programs are refused with `ENOEXEC` for now (M3). Build
programs static, with musl (`musl-gcc -static`) or glibc.

## What a Linux program sees

- **Start-up:** `argc`, `argv`, the environment and the auxiliary vector
  (`AT_PHDR`, `AT_ENTRY`, `AT_RANDOM`, `AT_EXECFN`, `AT_HWCAP`…) on an 8 MiB
  stack. The environment has `PATH`, `HOME=/home/<you>`, `USER`, `SHELL`,
  `TERM`, `LANG=C.UTF-8` and `XDG_RUNTIME_DIR`.
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
- **Threads:** `clone` for threads with thread-local storage (`arch_prctl`),
  `futex`, and the child-tid handshake `pthread_join` uses.
- **Everything else:** calls that aren't there yet return `ENOSYS`, and the
  first use of each is written to the system log (`linux: … system call N
  is not supported yet`).

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

`cargo xtask build` builds every `linux-tests/*.c` and Doom with `musl-gcc`
(Debian/Ubuntu: `sudo apt install musl-tools`) and downloads BusyBox from
Debian, pinned by SHA-256. Without `musl-gcc` the image is built without
Linux programs (with a warning), and the kernel's Linux self-test is skipped.

The self-test `linux-tests/linuxtest.c` runs inside WaveOS as part of
`cargo xtask test`: files, memory, threads, clocks, `/proc` and `/dev`, and
the WaveOS calls.
