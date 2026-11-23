# The Store and system updates

What exists: the Store app (`userland/apps/store`), signed repository
catalogues with freshness, trust roots and mirrors (`libs/gina/src/repo.rs`,
`userland/corekit/src/apps/repo.rs`), resumable verified downloads
(`userland/corekit/src/apps/download.rs`), app rollback, whole-system updates
in A/B slots with a boot-attempt counter (`libs/gina/src/system.rs`,
`userland/corekit/src/update.rs`, `bootloader/src/slots.rs`,
`kernel/src/gui/sysupdate.rs`), Settings → Software Update, background checks
with notifications, and the host tools that build repositories and updates
(`xtask/src/store.rs`). Packages themselves (GINA format 2, Ed25519) are
described in the GINA section of [WAVESDK.md](../WAVESDK.md).

No repository is configured out of the box: the address is a setting
(Settings → Software Update → Repositories, the Store's empty state, or
`pkg source add`). Every address can be `https://`, `http://` (everything is
signed either way) or a folder, `file:///Volumes/Stick/repo/`.

## Repositories

`cargo xtask repo FOLDER --sign KEY --out DIR` turns a folder of packages
into a signed repository; `cargo xtask serve-repo [DIR]` serves one to a
guest (`http://10.0.2.2:8000/`, with byte ranges). The folder holds the
`.gina` files (any number of versions) and, optionally, per app id,
`<id>/listing.txt` (`featured=1`, `about=…`, `notes.<version>=…`, one line
each with `\n` for new lines) and `<id>/screenshots/*.png` (up to eight, in
name order). The sample apps' listings are in `assets/store/`.

```text
catalog.txt, catalog.sig      the catalogue (format 2): what the Store reads
index.txt, index.sig          the same packages in format 1, for older systems
root/<n>.txt, root/<n>.sig    trust roots (optional)
packages/<id>-<version>.gina
assets/<sha256>.png           icons (taken from the packages) and screenshots
system/<version>/update.txt   system updates, signed by the system update key
system/objects/<sha256>       their files, shared between versions
```

The catalogue (`gina-repository 2`, see `gina::repo` for every line) adds to
format 1's `name`, `generated` and ten-field `package` lines: a `version` that
only grows, an `expires` time, `mirror` addresses, `featured` apps, four more
`package` fields (installed size, permissions, minimum system, publish time),
`icon` and `screenshot` lines naming pictures by path, SHA-256 and size (so
the catalogue's signature covers them), `about` and `notes` text, `revoked`
versions and `system` updates. Rewriting a repository makes the next
catalogue version; it expires after 30 days unless `--expires-days` says
otherwise, so a repository must be re-signed at least that often.

### What the client checks

`corekit::apps::repo` (used by the Store, `pkg` and Software Update):

- **Signatures.** `catalog.sig` holds one `ed25519 <key> <signature>` line
  per signer, over `GINA-2 repository catalog\0` and the bytes. One must be
  by a `catalog` key of the repository's current trust root, or, for a
  repository without roots, by a key the device trusts directly
  (`/System/Keys`, `/Applications/.keys`).
- **Trust roots and key rotation.** `root/1.txt` names `root` and `catalog`
  keys and must be signed by a directly trusted key. Root *n*+1 must be
  signed by a root key of root *n* and by one of its own. The client walks
  forward from the last root it accepted (kept per account in
  `~/Library/Packages/Roots/`), so a repository moves its signing key by
  publishing the next root; a device never falls back to direct trust for a
  repository once it has seen a root. An expired root (`expires`, 0 for
  never) is refused.
- **Freshness.** Each account remembers, per repository, the highest
  catalogue version and that catalogue's hash
  (`~/Library/Packages/repositories`). An older version (a rollback attack:
  bringing back vulnerable or revoked versions), the same version with other
  contents, and a catalogue past `expires` (a freeze attack: hiding updates)
  are refused. A repository that once served a catalogue is never read in
  format 1 again.
- **Mirrors.** Signed `mirror` lines name other addresses serving the same
  files. Files are fetched from the repository first, then each mirror; the
  last good catalogue's mirrors are also tried when the repository itself
  can't be reached. Since nothing unverified is used, mirrors are only
  transport.
- **Downloads** (`corekit::apps::download`) go to `<dest>.part`, hashed as
  they arrive. A broken connection, a pause or a restart continues with
  `Range: bytes=<n>-` (a 206 that must start exactly there; a 200 starts
  over); the result is renamed into place only when its size and SHA-256
  match the catalogue, otherwise it is deleted. Packages are downloaded per
  account into `~/Library/Packages/Downloads/`, pictures into
  `~/Library/Caches/Packages/Pictures/`.
- **Verification before unpacking.** A package is checked against the
  catalogue (size, hash), then by the installer (structure, every file's
  hash, the publisher's signature, key continuity with the installed
  version, no downgrade) before a single file is written.
- **Offline.** The last good catalogue of each repository is kept in
  `~/Library/Caches/Packages/<hash>/` and checked again (signature, expiry)
  when read, so the Store opens at once and browses offline.

### Installing, atomically

`corekit::apps` writes a new version to `<root>/<id>/.pending-<pid>`,
flushes it, renames it to `<version>-<sha256>`, then moves the `active`
pointer (new file, flush, rename). The version it replaced stays, named by a
`previous` pointer, so `apps::rollback` (`pkg rollback ID`, the Store's
"Revert to …") can switch back the same way after re-checking its hashes.
Interrupted anywhere — the installer killed, the power cut — the old version
or the new one is live, never a mix; `apps::recover` (run by the Store when
it opens, and by every install) clears staging folders of installers no
longer running. Apps for everyone go to `/Applications` (an administrator,
or the elevation prompt); "Only for me" goes to `~/Library/Applications`.

## The Store

A WaveUI app. Its sidebar has Discover, the categories that have apps,
Updates (with a count) and Library, a search field, the repositories' status
and a progress summary.

- **Discover**: a featured banner (the catalogues' `featured` apps,
  cross-fading every few seconds), New and Updated, Games, Apps, and
  category buttons. Categories and search show grids of cards.
- **App pages**: icon, name, developer, category, Get / Update / Open with
  the scope (For everyone, Only for me), version, size, minimum system,
  screenshots, About, What's New (the version's `notes`), the permissions the
  manifest declares (with the installer's wording, sensitive ones marked),
  the repository and its catalogue version, the versions offered, and for an
  installed app: where it is, Open, Remove (keeping or deleting its data)
  and Revert to the previous version.
- **Downloads**: one queue worked by a background thread; each download
  shows a ring and can be paused, resumed (from where it stopped, also
  after quitting) and cancelled (its partial file deleted).
- **Updates**: Update All, each app's release notes, WaveOS itself (opens
  Software Update), and installed versions a repository withdrew.
- **Library**: what is installed, for whom, with Open and Remove.
- **States**: an empty state that adds a repository when none is set, an
  offline banner (cached catalogues), repository errors, and loading
  placeholders that shimmer while pictures arrive.
- **Motion and input**: pages rise and fade in on a `lumen::spring` (the
  desktop's springs; reduced motion respected); pictures are fetched,
  verified, decoded and scaled on a background thread with a memory cache in
  front of the disk cache. Arrows move between cards and rows, Enter opens,
  Escape goes back; a game controller's d-pad, A and B do the same, and the
  shoulder buttons move between sections. In Wave Play the Store is a tile
  (Get More Games opens it on its games) and installed games join the
  library through the installed-apps source.

`Store.elf --category game`, `--app ID` and `--updates` open on a page;
`Store.elf --check` is the background check (below).

## Whole-system updates

The system — kernel, bootloader and the read-only system image mounted at
`/System` — is replaced as a unit.

### Format

A catalogue's `system` line names `system/<version>/update.txt`
(`gina::system::Update`): version, `min_from`, release notes, the SHA-256
and size of the kernel, bootloader and whole `system.tar`, then the system
image entry by entry (`dir` and `file` lines with each file's hash). It is
signed (`update.sig`, over `GINA-2 system update\0` and the text) by a key
in `/System/Keys/Updates` — never an app publisher's — and its own hash is in
the signed catalogue.

**Per-file deduplication.** Every file is an object,
`system/objects/<sha256>`. The device compares the new description with the
running system's (`\aurora\update.txt`, which the build writes for the EFI
partition's system, or the running slot's) and downloads only the objects
it doesn't have: an update that changes one file downloads one file. The new
`system.tar` is rebuilt entry by entry with the canonical headers both the
build (`xtask/src/tar.rs`) and the device use (`gina::system::tar_header`),
and must hash to the description's value. (A binary delta within changed
files is not done.)

`cargo xtask system-update VERSION --out DIR` adds an update built from the
checkout's EFI partition (`version.txt` says VERSION), signed with the
build's system update key (`target/keys/waveos-system.key`, made on first
use and trusted through `/System/Keys/Updates/waveos-system.pub`); rewrite
the catalogue with `cargo xtask repo` so it lists it.

### A/B slots on the home volume

The design note proposed both slots on the EFI partition, but the partition
is 64 MiB and one system already takes about 62 MiB (a 58 MiB image and a
5–6 MiB kernel), so two can't fit, and existing disks can't grow it without
moving the home volume. (The one system no longer fits uncompressed either:
`xtask/src/image.rs` writes `system.tar.zlib` instead when `system.tar`
doesn't fit, which the bootloader already inflated for optical discs.) The
slots are therefore folders on the home volume, which the bootloader reads
itself:

```text
esp   \aurora\kernel.elf + system.tar on the EFI partition (as installed)
a, b  /.system/a/, /.system/b/: kernel.elf, system.tar, BOOTX64.EFI,
      update.txt (the description its files are checked against)
```

`/.system` belongs to the system with the admin group allowed to write, and
like `/Applications` nobody else may create, remove or rename it. The home
volume's own files are never part of an update.

**Installing** (`corekit::update::install`, administrators only) writes the
slot that isn't running into `/.system/.staging-<pid>`: files the running
system has are copied from it (and re-hashed), the others come from the
downloads (`~/Library/Caches/System Update/`, written without privileges).
Then every file is read back and checked, the folder is renamed to its slot
and `/.system/ready` names it. Nothing about the next start has changed yet.

**Arming** (Restart Now, Update and Restart in the power dialog, or
`pkg system restart`) rewrites `\aurora\boot.conf` (new file, rename):
`system=<slot>`, `system_tries=3`, `system_previous=<running slot>`.

**Starting** (`bootloader/src/slots.rs`). The bootloader reads
`boot.conf`. For a slot on trial it takes a try first and writes the count
back before loading anything, so a crash at any point still counts; with no
tries left it starts `system_previous` instead. It reads the slot's files
from AuroraFS through the disk's Block I/O protocol (the partition on the
same disk as the EFI partition first), mounting the volume over an overlay
that keeps the journal replay in memory, so the volume is never written; on
a trial start it checks the kernel and image against the slot's
`update.txt`. A slot whose files are missing or don't match is skipped the
same way, the EFI partition's own system being the last resort. When it
goes back it writes `system=<the one started>` and `system_failed=<slot>`.
The slot started is passed to the kernel in `BootInfo.flags`
(`BOOT_SLOT_A`, `BOOT_SLOT_B`, and `BOOT_SLOTS` from a bootloader that knows
slots; the layout of `BootInfo` is unchanged, so an older bootloader still
starts a newer kernel).

**Marking good** (`kernel/src/gui/sysupdate.rs`). When the desktop draws
its first frame, the kernel removes `system_tries`: the update has proved
it starts. Only then is the bootloader the update shipped copied over
`\EFI\BOOT\BOOTX64.EFI` (new file, read back, rename), so a new bootloader
never runs before its system has started once with the old one. (When the
running bootloader doesn't know slots, the update's bootloader is installed
when arming instead.) If the desktop never comes up, the bootloader runs out
of tries on its own and the previous system starts; Software Update then
says that version couldn't start.

`cargo xtask run` refreshes the EFI partition of the developer's disk with
the new build and drops the `system*` lines from `boot.conf`, so a fresh
build always starts, whatever was installed through Software Update.

### Software Update and notifications

Settings → Software Update shows the running version and slot, checks,
shows what's new and the download size, downloads (pausable and resumable),
installs, and restarts; it reports a version that failed to start, and has
the switches "Check for updates automatically" (`software_update_check`, on)
and "Download system updates automatically" (`software_update_download`,
off), and the list of repositories. The desktop starts `Store.elf --check`
three minutes after it comes up and every six hours (the Store checks at
most once in 20 hours): it posts one notification for app and system
updates not mentioned before, which opens the Store's Updates. The WayV
menu's Restart becomes Update and Restart while an update is ready.

## Tests

- Host: `cargo test -p gina` (catalogues, roots and rotation, freshness,
  system update descriptions and image rebuilding, fuzzed parsers),
  `-p nebula-web` (ranged streaming downloads), `-p xtask` (repository
  writing, broken kernels, retagged images).
- `cargo xtask test` builds `target/test-repo` (`store::test_repositories`)
  and serves it with copies that misbehave on purpose (`repo_response` in
  xtask: altered, damaged, cut short, dropping the connection halfway,
  slow, stale, expired, rotated). `pkgtest` and the kernel check, across
  the run's starts: install, update, rollback and removal; the catalogue
  altered after signing, rolled back, expired, and signed by a rotated key;
  damaged and cut-short downloads refused before anything is unpacked; a
  dropped download resumed with a range; pausing, resuming and cancelling;
  an install killed half-way (nothing half-installed, recovery cleans up);
  the power cut in the middle of an install on the first start (the harness
  quits QEMU; the second start finds the old state or the new one); a
  system update downloaded (one object), installed into slot a and started
  on the second start, marked good; and an update whose kernel faults at its
  first instruction, armed with one try, which the bootloader abandons on a
  third start, going back to slot a.
- `tools/smoke_store.py` drives the Store in a running guest on a
  throwaway disk, against `cargo xtask serve-repo --slow` (packages sent
  slowly, so downloads can be watched): first-start setup, the empty state,
  adding the repository, Discover, search, an app's page, Get (downloading,
  installed), Open, the Library, Remove, Updates and Software Update, with
  a screenshot at each step. Pictures from it are in
  [docs/screenshots/store](../screenshots/store/).

## Not done

- No binary deltas inside changed files; no compression in transit.
- The bootloader checks slot files against `update.txt` but not the
  update's signature (no Secure Boot chain); anyone who can write `/Boot`
  or `/.system` is an administrator already.
- Read-back verification reads through the kernel's block cache, so it
  proves the filesystem's view, not the medium's.
- A bootloader that fails after it was installed can't be undone
  automatically (it is installed only after its system started once).
- The Store doesn't manage Debian packages for `/Linux`.
- Per-app sandboxing (see [sandboxing.md](sandboxing.md)) should land before
  third-party repositories are encouraged.
