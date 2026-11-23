# Memory pressure: what apps see when memory runs short

WaveOS doesn't stop handing out memory when RAM is full. As on macOS and
Windows, it first takes back what the system can rebuild, then moves pages
of programs that haven't touched them lately out of RAM — compressed in
memory, then to an encrypted swap file — and brings each back the moment
it is touched. Only when that can't keep up does it end a program. How it
works inside is in [ARCHITECTURE.md](../ARCHITECTURE.md) ("When memory runs
short"); this page is what an app can see and do.

## What happens to your app

- **Nothing you have to handle.** Your memory may be swapped out while you
  aren't using it; touching it brings it back (slower the first time:
  tens of microseconds from compressed memory, more from disk). Contents
  never change.
- **`mmap` may fail** with `ENOMEM` when memory and swap are both full. A
  native `mmap` gives you all of the memory at once or fails; it never
  hands out memory it can't back.
- **Your app may be ended** if memory runs out and reclaim can't keep up.
  The out-of-memory killer picks by how much memory ending a program frees
  (in RAM and swapped out), counting only an eighth of it for the app the
  user is using and half for apps with windows, so a big app in the
  background goes first. The user gets a notification naming the app.
  It is a `SIGKILL` (exit code `-9`): no chance to save, so save early.
- **Locked memory** (Linux `mlock`/`mlockall`) is never swapped out. Use
  it for secrets you don't want on disk (they would be encrypted there,
  but still), not to protect performance: it takes RAM from everyone.

## Being a good citizen: the pressure event

Every window gets `event::MEMORY_PRESSURE` when the level changes:

| `a` | Meaning | What to do |
|---|---|---|
| 0 | No pressure | Nothing. |
| 1 | Warning: memory is short and being reclaimed (or swap is over half full) | Let go of caches you can rebuild: decoded images, undo history beyond a few steps, prefetched data. |
| 2 | Critical: programs are waiting for memory, and one may be ended soon | Free everything you can, and save the user's work. |

The level goes up at once and comes down only after ten quiet seconds.
`sys_stats` (`SysStats`) has the same level (`mem_pressure`), the share of
the last ten seconds some program spent waiting for memory
(`mem_stall_10s`), and the figures Pulse shows: available memory,
compressed memory and what it holds, swap and swap-file use, pages
swapped in and out, and programs ended for memory.

## Linux programs

They see what they would on Linux: `/proc/meminfo` (with `SwapTotal`,
`SwapFree`, `SwapCached`, `Zswap`, `Zswapped`), `/proc/pressure/memory`
("some" only; "full" reads as zero), `/proc/<pid>/status` (`VmRSS`,
`RssAnon`, `RssShmem`, `VmSwap`, `VmLck`), `sysinfo` with swap,
`madvise` (`MADV_DONTNEED` and `MADV_FREE` free pages now, `MADV_PAGEOUT`
swaps them out now, `MADV_COLD` makes them first to go, `MADV_WILLNEED`
brings swapped-out ones back, `MADV_POPULATE_READ`/`WRITE` make them), and
`mlock`, `mlock2`, `munlock`, `mlockall`, `munlockall`. `mlockall`'s
`MCL_FUTURE` is accepted, but memory mapped later isn't locked.

## Testing your app under pressure

`cargo xtask run --memory-mib 512` gives the VM little memory. The kernel
self-tests do the same at any size (`cargo xtask test --memory-mib 256`,
`AURORA_TEST_FILTER=swap` for only the swap tests, `AURORA_SWAP_TEST_RAM`
and `AURORA_SWAP_FACTOR` for how much they fill).
