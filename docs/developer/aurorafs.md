# AuroraFS: what your files can count on

AuroraFS is the filesystem of the home volume (`/`): WaveOS's own,
journaled, in `libs/aurorafs` (shared by the kernel, the bootloader and the
host tools in `xtask`). The on-disk format is in
[ARCHITECTURE.md](../ARCHITECTURE.md) ("AuroraFS on-disk format"). This page
says what an app, a Linux program or the system can rely on, how the journal
delivers it, and how it is tested.

## The guarantees

- **Every operation is atomic.** Creating, removing, renaming (also over an
  existing file, and across folders), writing, truncating, changing an
  owner or a mode, setting times: after a crash the volume holds the tree
  as it was after some operation, never half of one. A rename over a file
  leaves the old file or the new one, never neither.
- **What was synced is there.** When `sync`, `fsync`, `fdatasync` or
  `syncfs` returns (native `SYNC`; Linux calls of those names), every change
  made before it, to any file on the volume, is on the disk. Without them,
  changes are committed every 5 seconds (the `flusher` task), at shutdown
  and restart, before sleep, and whenever a transaction grows past half the
  journal.
- **No file ever shows another file's data.** File data is written in place
  and flushed before the metadata that points at it commits ("ordered
  mode"). After a crash, a file's size and name are those of a committed
  operation; data you *overwrote* in place after that may show the old or
  the new bytes, sector by sector, as with any filesystem in this mode.
  Data a file never had (the blocks of a deleted file) is never visible,
  with one exception: the swap file's blocks (`Volume::allocate`), which
  the swap code writes before it reads.
- **A failed call changes nothing.** A call that fails part-way (the disk
  is full, the file has too many extents, a read error) puts back every
  change it made to the metadata. A full disk never leaves an inode or a
  block behind.
- **A file removed while open stays open.** Its descriptors can still read,
  write and truncate it; its inode and blocks are freed at the last close.
  No new file or folder gets its inode in the meantime. If the power goes
  first, the next start frees it.
- **A folder can't go inside itself.** Moving a folder into itself or
  anything below it fails (`EINVAL`) whatever paths the caller checked; a
  folder that was removed while open takes no new entries (`ENOENT`).
- **The disk is checked after an unclean shutdown.** A volume that was not
  cleanly unmounted (shutdown and restart unmount it cleanly; a power cut
  or a reset doesn't) is checked at the next start, after its journal is
  replayed. Files left open at the cut are freed quietly. Anything else is
  repaired where that is safe, the user gets a notification ("The disk was
  checked"), and the system log lists every problem. Nothing is thrown
  away: a file or folder no folder names any more is put back under
  `/lost+found`.

## How the journal does it

A volume has one journal slot (1/16 of the volume, 32 to 1024 blocks).
Every metadata change (superblock, bitmaps, inodes, extent blocks, folder
contents) goes into the running *transaction*, an overlay of whole block
images that reads also see. `Volume::commit`:

1. flushes the device, so file data written in place is durable before
   anything points at it, and the previous transaction's home locations
   are too;
2. writes a descriptor (sequence number, home block numbers), the images
   and a commit record to the journal, and flushes;
3. copies the images to their home locations ("checkpoint"), which the
   next commit's first flush makes durable. Until then the journal still
   holds the transaction, and replaying it repairs any home write a crash
   tore.

The commit record carries two checksums over the descriptor and the
images: CRC-32 (all versions) and CRC-32C (since 0.8.1, marked by `C32C` at
byte 28). CRC-32 alone was not enough: a superblock ends its checked part
with its own CRC-32, and CRC-32 over any data followed by its CRC-32 is the
same, so a power cut that landed the next transaction's superblock image in
the slot of the last one (before the new descriptor) passed the old
record's check, and replay put the new superblock over the old bitmaps.

On mount, a transaction whose descriptor, commit record and checksums all
match is replayed: its images are written home (every home must lie inside
the volume and outside the journal), the device is flushed, and the
descriptor is zeroed. A torn one is ignored. Replaying twice does no harm
(the images are whole blocks), so a crash during replay is fine too.

**Operations never span commits.** `Volume::op` runs each public call as
one operation, with an undo log of what it changed (block images, bitmap
bytes, the superblock, blocks freed); a commit happens only between
operations. An operation that fails is rolled back. One that would not fit
in what is left of the journal is rolled back, the running transaction is
committed, and it runs again; one too big for the whole journal (rewriting
a folder of more than about a megabyte of entries, half the journal) fails
with `EINVAL` and changes nothing. After an operation, the transaction is
committed if it is within 64 blocks of filling the journal (past half, on
a journal under 128 blocks).

**Freed blocks wait.** Blocks freed in a transaction are not reused until
it commits, and freeing a block revokes any image of it in the running
transaction. Together with the single journal slot, this means a replay
can never write an old folder or inode image over a block that has since
become file data.

**Flushes are the only barrier.** The block cache (`kernel/src/fs/cache.rs`)
is write-back: it may write a dirty block to the device at any time
(eviction), in any order. `flush` writes every dirty block and then flushes
the device; nothing is written with FUA. The journal's ordering needs only
that a flush makes everything written before it durable, and that a write
the device acknowledged before a flush is not lost after it. Each driver's
flush:

| Controller | Flush | Notes |
|---|---|---|
| AHCI (SATA) | `FLUSH CACHE EXT` (0xEA), waited for | |
| NVMe | Flush command on the I/O queue, waited for | |
| virtio-blk | `VIRTIO_BLK_T_FLUSH`, waited for | `VIRTIO_BLK_F_FLUSH` is negotiated (since 0.8.1), so the device may cache writes; before, QEMU wrote through, flushing the host file after every write (one A/B under TCG, `cargo xtask powercut --disk virtio --iterations 15`, same seed: 154 synced files under load with the flush feature, 135 without) |
| SDHCI (eMMC) | `SWITCH` to `FLUSH_CACHE` when the card's cache is on, then wait while busy | |
| SDHCI (SD) | wait while the card is busy | SD cards have no cache to flush |
| USB mass storage | `SYNCHRONIZE CACHE(10)` | a device that refuses it is taken to have no cache |

A flush with nothing written since the last one costs nothing (the cache
skips it), so an `fsync` of an unchanged volume is free.

**Clean unmount.** `Volume::unmount` (the kernel calls it for every volume
through `fs::unmount_all` before shutting down or restarting) commits and
sets a "cleanly unmounted" mark in the superblock extension (byte 200,
`LEAN`). Mounting clears it with its first commit, so it is only there
while nothing has changed since. An older kernel writes zero there, which
reads as "not clean": the safe direction.

## Checking and repairing

`Volume::check` walks the tree from the root and reports every problem it
finds, not just the first: entries naming free or unreadable inodes,
folder contents that stop parsing, invalid or repeated names, an inode
reached twice (a second name, a folder inside itself), wrong link counts,
allocated inodes no folder names (an *orphan* if it has no links: a file
removed while open), blocks claimed twice, blocks in use but free in the
bitmap, blocks marked used that nothing uses, metadata blocks marked free,
and free counts that disagree with the bitmaps. `Volume::fsck` is the same
as one line of text.

`Volume::repair` fixes what can be fixed without guessing: it drops bad
entries, cuts folder contents at the first record that doesn't parse,
corrects kinds, link counts, sizes beyond the blocks, unknown flags, the
bitmaps and the free counts, frees orphans, and puts other unreachable
files and folders back as `/lost+found/#INODE`. A block claimed by two files
is reported and left alone (no repair is safe). It commits after each
pass and checks again.

On the host:

```
cargo xtask fsck target/test-disk.img          # every problem; the image is not changed
cargo xtask fsck some.img --repair             # repair, written back (keep a copy first)
```

`xtask fsck` mounts through an in-memory overlay, so even the journal
replay leaves the image as it was. Never run `--repair` on a disk you can't
afford to lose without a copy.

## Testing

- `cargo test -p aurorafs` runs, besides the unit tests:
  - `stress.rs`: random sequences of operations (create, write, truncate
    up and down, rename over files and across folders, folders into
    themselves, remove, remove while open and keep writing, owners, times,
    the swap file's `allocate`, sync) against a reference model, checking
    `fsck` and the whole tree after every operation; then a simulated power
    cut at every write boundary, keeping what was flushed plus any subset of
    the rest, old or new versions, some torn at 512-byte sectors. Each image
    is mounted (replaying the journal), checked, repaired of orphans only,
    and must match the model after one of the operations since the last
    sync. `AURORAFS_STRESS_SEEDS`, `_OPS`, `_SEED`, `_CUTS` make it longer;
    run long ones with `--release`. A failure is shrunk to a minimal
    sequence and printed.
  - `concurrent.rs`: four threads making the kernel VFS's sequences of calls
    (a lookup per path component, then the operation; descriptors kept
    across removals) through a scheduler that picks who takes the volume
    lock next from a seed, so a bad interleaving is found again. Besides
    `fsck`, one thread writes files nobody else names, whose contents must
    survive whatever the others' descriptors do. The fuzz seeds of failing
    runs are replayed first. `AURORAFS_RACE_SEEDS`, `_OPS`, `_THREADS`.
  - `regressions.rs`: the minimal reproducers of the bugs below.
- `cargo xtask test` checks the home volume with `fsck` after each fuzzer
  (`home_volume_ok`), checks it on the host after the first boot's power
  cut, and the second boot asserts the check at mount found nothing but
  orphans. The fuzzers' scratch folders are left as they are: the next
  boot fuzzing on top of them is a test too.
- `cargo xtask powercut --iterations N` boots the test kernel N times on
  one disk with four threads hammering the volume (`fs_stress`), cuts the
  power at a random moment, checks the volume on the host each time
  (orphans aside), checks that every file the guest synced is there and
  whole, and lets the next start mount the cut disk (whose own check must
  find nothing but orphans). `AURORA_POWERCUT_SEED` repeats a run,
  `--disk ahci|virtio|nvme` picks the controller and `--arch aarch64`
  runs it on arm64 (virtio only; with HVF on an arm64 Mac). Nothing in
  the library, the stress or the checks is architecture-specific.

## Bugs this found (0.8.1)

| Symptom | Cause | Fix |
|---|---|---|
| "dir 85: Corrupt" after a power cut mid-install | Operations committed in the middle: a folder's new contents were written by one call and its new size set by the next, and a transaction could commit between the two (on volumes under 64 MiB, after every call), leaving stale bytes past the entries | Operations are atomic (`Volume::op`) |
| "inode N allocated but unreachable" after a power cut | The same: a removal committed after the folder was rewritten and before the inode was freed | The same |
| "inode 199 allocated but unreachable" after the native fuzzer, no crash | Making a file on a full disk: the inode was allocated, the folder couldn't grow, the error left the inode | Failed operations are rolled back |
| A moved file lost on a full disk | A move rewrote the source folder, then failed to grow the destination | The same |
| "dir N: Corrupt" or unreachable inodes while the fuzzers run | A descriptor kept across its file's removal: the inode number went to a new folder, and a truncate through the old descriptor cut the folder's contents | Open inodes are pinned (`open_ino`); the library refuses to write or truncate a folder |
| A file synced and never touched again came back holding another file's data (`cargo xtask powercut`, cut 32) | `open` looked the path up, then pinned the inode in a second call: in between, another thread removed the file and a new file got its inode, which the descriptor then held and wrote | `open` resolves the folder, then looks the name up and pins it in one call (`Filesystem::open_at`, `Volume::open_at`); the race harness now checks files' contents, not just the structure |
| Subtrees cut off by racing renames | The VFS checks "not into itself" on paths; a racing rename made them stale, and the library did the move | The library checks, inside the operation |
| Free counts disagreeing with the bitmaps after a torn commit | CRC-32 can't tell superblock images apart | CRC-32C as well |

On-disk changes are compatible both ways: the "cleanly unmounted" mark and
the CRC-32C are read by this version and ignored by older ones; a volume
written by an older kernel mounts (and is checked once, as not clean).
