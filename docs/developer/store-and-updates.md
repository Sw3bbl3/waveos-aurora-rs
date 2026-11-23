# A graphical Store and whole-system updates: design note

Status: design only. What exists: GINA format 2 packages (signed, hashed;
`libs/gina`), the installer, registry and repositories (`corekit::apps`,
`corekit::apps::repo`), `pkg`, Installer and Settings → Apps. See the
GINA section of [WAVESDK.md](../WAVESDK.md).

## Store

A WaveUI app (`userland/apps/store`) on top of `corekit::apps::repo`; it adds
no trust of its own.

- **Browse and search**: `repo::indexes()` already gives every configured
  repository's signed index. The index needs, per package, an icon and
  screenshots: add `icon` and `screenshots` fields (paths in the repository,
  each with a SHA-256, so the index signature covers them) as new tab fields
  behind `gina-repository 2`. Old clients ignore version 2 indexes; new ones
  read both.
- **Pages**: name, publisher (and whether its key is trusted), description,
  categories, requested permissions (the same wording as Installer, from
  `gina::describe_permission`), size, version history.
- **Install / update / remove**: `repo::install`, `repo::updates`,
  `apps::uninstall`, with the progress callback; installing for everyone
  goes through `apps::authorize` (the administrator prompt) as today.
- **Updates**: a background check (once a day, only with a configured
  repository and a network) posts a notification "3 updates"; the Store's
  Updates tab applies them. Key continuity and no-downgrade already hold.
- **Games**: Wave Play lists installed games with
  `apps::in_category("game")` (or `/Applications/.registry`); its "Get more
  games" opens the Store filtered to `game`.
- **Curation**: a default repository run by the project, its key in
  `/System/Keys`; third-party repositories stay opt-in (`pkg source add`).
  Revocation: the index gains a `revoked<TAB>id<TAB>version` line; clients
  refuse to install those and Settings → Apps flags installed ones.

## Whole-system updates

The system image (`/System`, a read-only `system.tar`) and the kernel are
replaced as a unit, never file by file.

- **Format**: a system update is a signed bundle: `kernel.elf`,
  `system.tar` (or a binary delta against the running one), `BOOTX64.EFI`,
  and a manifest with the version, minimum version to update from, and a
  SHA-256 of each part, signed with the project's system key (separate from
  app keys; its public half in the bootloader and in `/System/Keys`).
- **A/B on the EFI partition**: `\aurora\a\` and `\aurora\b\` each hold a
  kernel and system image; `\aurora\boot.conf` names the active slot and a
  `tries` counter. The updater writes the inactive slot, verifies every
  hash after writing (reading back), flushes, then flips `boot.conf`
  atomically (write new, flush, rename on FAT32 via the existing
  `fs::write_atomic`).
- **Rollback**: the bootloader decrements `tries` before booting a new
  slot; the system marks the boot good once the desktop has started
  (`tries` cleared). A slot that fails to come up three times is abandoned
  and the bootloader boots the other one.
- **Delivery**: the same repository mechanism (`gina-repository` index
  with a `system` line naming the bundle, size and hash), so mirrors,
  rollback protection (`generated`) and HTTPS are shared with apps.
- **Privilege**: applying a system update takes the administrator
  capability; downloading may run unprivileged into a staging folder.
- **The home volume** is untouched; migrations of on-disk formats run at
  first boot of the new version and must be idempotent.

## Open questions

- Delta format for `system.tar` (bsdiff-style vs. per-file) and its cost
  on the soft-float kernel side (it would run in user space).
- Whether the Store should also manage Debian packages for `/Linux`
  (today `xtask` builds that tree on the host).
- Per-app sandbox enforcement (see [sandboxing.md](sandboxing.md)) should
  land before third-party repositories are encouraged.
