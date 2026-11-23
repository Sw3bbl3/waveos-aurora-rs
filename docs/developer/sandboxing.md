# Per-app sandboxing: design note

Status: design only. Accounts, file owners and the administrator capability
(`kernel/src/users`, `kernel/src/fs/perm.rs`) are the foundation this builds on.

## Where we are

Every process carries credentials (`users::Cred`): a uid, a primary group,
supplementary groups, the administrator capability (held by administrator
accounts, or granted for five minutes by the system prompt) and a umask.
They are inherited on spawn and `fork`, and the VFS checks Unix permission
bits against them. That isolates *accounts* from each other. It does not
isolate *apps* of one account: a game Alex runs can read Alex's Documents,
open sockets and change Alex's preferences, because it acts as Alex.

## The proposal: a capability set attached to credentials

Add one field to `Cred`:

```rust
pub struct Sandbox {
    /// What the app may do beyond computing and drawing its own windows.
    pub caps: u64,               // bit set, see below
    /// Folders it may read (and, separately, write), as path prefixes.
    pub read: SmallVec<[PathId; 4]>,
    pub write: SmallVec<[PathId; 4]>,
}
pub struct Cred { /* … */ pub sandbox: Option<Arc<Sandbox>> }
```

`None` means unconfined (today's behaviour, and the default for system
tools). A sandbox can only ever be narrowed: `cred_set` and spawning may
drop capabilities or folders, never add them, so a child of a sandboxed app
is at most as able as its parent — the same rule `cred_set` already follows
for the administrator capability.

Capability bits (first set):

| Bit | Allows |
|---|---|
| `NET_CLIENT` | outgoing TCP/UDP (`socket`, `connect`) |
| `NET_SERVER` | `bind`/`listen` on non-loopback addresses |
| `AUDIO_OUT`, `AUDIO_IN` | `audio_open`, a future capture call |
| `GAMEPADS` | `gamepads`, `gamepad_state`, rumble |
| `CLIPBOARD_READ` | `clipboard_get` (writing stays allowed) |
| `NOTIFY` | `notify` |
| `PREFS_WRITE` | `pref_set` |
| `SPAWN` | `spawn` of programs other than itself |
| `LINUX` | running Linux programs (their syscall surface is large) |
| `ELEVATE` | asking for the administrator capability at all |
| `USER_FILES` | the whole home folder (otherwise only `read`/`write` lists) |

## Enforcement points

- **Files**: `fs::perm::caller()` already returns the credentials every VFS
  entry point checks. A sandbox adds one test after the permission bits: the
  normalised path must lie under a granted prefix (plus `/System`, read-only,
  and the app's own container `~/Library/Containers/<app id>`, always
  writable). Because all native and Linux file calls funnel through
  `fs::{lookup_full, parent_full, open_mode, readdir}`, there is one place to
  add it; `fs::perm::system` stays the only bypass, used by kernel code for
  the system's own files.
- **Other calls**: a small table in `syscall::handle` and `compat::linux`
  mapping call numbers to capability bits, checked before dispatch (`ENOSYS`
  stays `ENOSYS`; a missing capability is `EACCES`).
- **The desktop**: requests that make the window server act for an app
  (`desktop::OPEN_APP`, `OPEN_FILE`, `TRASH`) must carry the requester's
  credentials into the `Command`, so an app can't launch something less
  confined than itself through the desktop. Today the desktop starts apps
  with the session's credentials.
- **User-chosen files**: a system open/save panel (built into the window
  server, like the administrator prompt) grants the chosen path to the
  asking process for its lifetime by adding it to `read`/`write` — the
  "powerbox" pattern, so sandboxed apps still open any document the person
  picks.

## Where the set comes from

GINA app manifests (`gina::Manifest`, installed under `/Applications` or the
home folder) gain an `[permissions]` table naming capabilities and folders.
The installer shows them before installing; the kernel builds the `Sandbox`
when it spawns a program whose path is inside an installed app's folder,
reading the manifest the package manager verified. Programs in `/System`
keep `None`. Settings gets a per-app page to revoke grants.

## Open questions

- Path prefixes vs. inode-based grants (renames move files out of a grant).
- Whether Linux programs get a separate, coarser profile.
- Auditing: a log of denied operations per app, for the Settings page.
