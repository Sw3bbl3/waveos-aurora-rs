# Per-app sandboxing

Accounts (`kernel/src/users`, `kernel/src/fs/perm.rs`) keep *people* apart:
every process acts as an account and the VFS checks Unix permission bits.
That does not keep one person's *apps* apart — a game Alex runs could read
Alex's Documents, open sockets and record the microphone, because it acts as
Alex. The sandbox (`kernel/src/sandbox`) adds the second wall, as on macOS
and iOS: an app can touch only what its manifest declares and what its
person allowed or handed it.

## The model

Every process may carry a `sandbox::Sandbox` (`Process::sandbox`; `None`
is unconfined):

- its app's **profile** (`sandbox::Profile`), one per app and account: the
  app's id and name, its *container*, its own files (the package), what its
  manifest **declares**, and where the rules come from (`Kind`);
- the capabilities it **gave up** (`sandbox(RESTRICT)`), never to return;
- the files and folders the person **gave** it (grants).

Whether a declared capability may be used is decided at the moment of use:

| Declared? | Guarded? | The person said | Result |
|---|---|---|---|
| no | – | – | refused (`EACCES`) |
| yes | no | nothing, or allow | allowed |
| yes | – | deny (Settings) | refused |
| yes | yes | nothing yet | **asked** (the privacy prompt), answer remembered |
| yes | yes | allow | allowed |

Guarded capabilities are the microphone, camera, screen capture, input
monitoring, raw USB, and the folders. The person's answers live in
`/Security/Privacy/<uid>.conf` (the system's, mode `0600` in a `0700`
folder, like the account database); they are looked up on every check, so
switching something off in Settings reaches running apps at once.

### Capabilities

One bit set, `aurora_abi::sandbox` (manifests use the names):

| Name | Bit | Allows |
|---|---|---|
| `network` | `NET_CLIENT` | `socket` (native, Linux `AF_INET`/`AF_INET6`) |
| `network-server` | `NET_SERVER` | `listen` on a network socket (a listener takes every interface) |
| `audio` | `AUDIO_OUT` | sound streams (`audio_open`, Linux `/dev/dsp`) |
| `microphone` | `MICROPHONE` | capture streams; checked again on every read |
| `camera` | `CAMERA` | reserved (no camera drivers yet) |
| `screen-capture` | `SCREEN_CAPTURE` | a handle (`GETFB`/`GETFB2`) to a framebuffer the program didn't make — the desktop's or another program's (framebuffer ids are global) — which it could map and read; without it the size and format, and showing it again by id, still work |
| `input-monitoring` | `INPUT_MONITOR` | reading keyboards and pointers (`/dev/input/event*`) while another program has the focus; grabbing them (`EVIOCGRAB`) without the focus; making keyboards and pointers with `/dev/uinput` (the default profile may make them without asking, as its account always could) |
| `gamepad` | `GAMEPADS` | `gamepads`, `gamepad_state`, rumble; controllers' `/dev/input/event*`, `js*` and `hidraw*`; making controllers with `/dev/uinput` |
| `usb` | `USB` | other raw HID devices (`/dev/hidraw*`); raw USB is reserved |
| `spawn` | `SPAWN` | starting other programs (they inherit the sandbox); Linux `execve` |
| `launch` | `LAUNCH` | asking the desktop to open apps and documents; starting another app's program as that app |
| `ipc` | `IPC` | other apps' local sockets and shared memory (otherwise a namespace of its own) |
| `notifications` | `NOTIFY` | `notify` |
| `background` | `BACKGROUND` | running on without a window |
| `clipboard` | `CLIPBOARD_READ` | reading the clipboard while another app is in front |
| `preferences` | `PREFS_WRITE` | `pref_set`, appearance, wallpaper, volume, the tour |
| `linux` | `LINUX` | starting Linux programs |
| `elevate` | `ELEVATE` | the administrator prompt at all |
| `display` | `DISPLAY` | the DRM card (`/dev/dri/card*`; render nodes are free) |
| `documents`, `downloads`, `pictures`, `music`, `movies`, `desktop` | `DOCUMENTS` … | that folder of the home folder (`:read` variants: read only) |
| `home` | `HOME` | the whole home folder, other apps' containers excepted |

The system's own apps may also declare `system` (power, displays, Wi-Fi,
the clock, the kernel log, the Trash and volumes), `admin` (keep an
administrator's capability), `privacy` (Settings' view of all this),
`processes` (signal and end the account's other programs), `linux-home`
(the real Linux home and `/tmp`) and `all-files`. `gina::Manifest::parse`
refuses them; only `parse_first_party`, used for the manifests built into
the kernel, accepts them.

### Files

All native and Linux file calls funnel through the VFS, which asks
`sandbox::check_path(path, Stat | Read | Write)` with the normalised path
(after the Linux layer resolved its links and mapped its tree):
`lookup_full` (any use: `Stat`), `open_mode` (`Read` or `Write`),
`readdir` (`Read`), `parent_full` (creating, removing, renaming: `Write` on
the entry, for both ends of a rename), `chmod`/`chown` (`Write`), `access`
and `space_of`. Kernel code doing the system's own work inside a call
(`fs::perm::system`) is not checked, as before.

A sandboxed process may use:

1. `/System`, read-only;
2. its container, everything;
3. its own package's files, read-only;
4. what it was given (grants), as given (read, or read and write), and
   for a folder everything in it;
5. the Linux system folders (`/Linux/{bin,etc,lib,lib64,opt,sbin,usr}`, the
   link table), read-only;
6. with `linux-home`, the account's Linux home and `/tmp`;
7. with `all-files`, anything its account may;
8. in the home folder (other apps' containers excepted): everything with
   `home`, or a declared folder (`documents`, `pictures:read`, …);
9. for `Stat` only, the folders on the way to any of the above (so `/` and
   `/Users/alex/Library` can be walked through, not listed).

Unix permissions still apply on top, always.

### The container

`~/Library/Containers/<app id>/`, the account's, `rwx------`, made by the
kernel when the app first starts:

```text
Documents/     the app's own documents
Preferences/   its settings (one file per thing it keeps)
Caches/        what it can make again (may be cleared)
tmp/           temporary files; its Linux programs' /tmp, /var/tmp and /run
Home/          its Linux programs' home folder ($HOME)
```

Apps find it without hard-coded paths: `corekit::sandbox::{container,
documents, preferences, preferences_file, caches, temporary}`, re-exported as
`aurorakit::container::{root, documents, …}`. They work unsandboxed too
(the container is then the one the program would have). The same folder
is `corekit::apps::data_dir`, which uninstalling may delete.

### What the person hands an app

No blanket grant is needed for what the person does themselves:

- **The open and save panel** (`sandbox(PANEL)`, `corekit::sandbox::
  {open_panel, save_panel, choose_folder}`): the window server shows its own
  panel (`gui/apps/panel.rs`, listing only what the account may read), so
  the app sees nothing but the answer; the asking process may then read and
  change exactly the file or folder picked.
- **Opening a document with an app** (double-click, Open With, a drop on the
  dock, `open_file` from another app): the desktop starts the app with the
  document granted.
- **Drag and drop**: files dropped on an app's window are granted when it
  reads the drop (`drag_data`).
- **The clipboard**: files pasted (read while the app is in front) are
  granted. An app may only put on the clipboard or drag files it may read
  itself, so it cannot launder a path through them.
- **Bookmarks**: `sandbox(BOOKMARK, path)` turns a grant into a 128-bit
  random token kept by the kernel (in the privacy file, per account and
  app). `sandbox(RESOLVE, token)` in a later run grants the same file again;
  another app's token, or a made-up one, is `ENOENT`. Settings lists and
  forgets them.

### Asking, Settings and the indicator

- **The privacy prompt** (`gui/apps/consent.rs`, like the administrator
  prompt): "“Example” would like to use the microphone." *Allow* and *Don't
  Allow* are remembered; dismissing it refuses this time only. The asking
  system call waits for the answer (two minutes at most). Nobody to ask (no
  window server, as in the self-tests) is a refusal that is not remembered.
- **Settings → Privacy & Security** lists, per capability, every app that
  declares it with a switch (and *Reset*, to be asked again), the files apps
  keep bookmarks to (*Forget*), and the latest refusals (the audit log,
  `sandbox::denied`, also in the kernel log once per app and thing).
- **The menu bar** shows an orange dot beside the right bubble while
  anything records from a microphone (`sandbox(IN_USE)`).

### Processes

- **Children inherit** a copy of their parent's sandbox (`fork`, `spawn`,
  Linux `clone`): same app, same given-up rights, same grants. Giving up
  rights later affects only the one giving them up.
- **Starting another app**: a sandboxed program with `launch` that starts
  another app's program (`/System/Apps/Voyage.elf`, an installed app) gets
  that app running *as itself*, in its own profile — as if the desktop had
  opened it — and passes on only the documents among the arguments it may
  use itself (as far as it may: read, or read and write). Anything else needs
  `spawn` and runs in the caller's sandbox. Starting its own program again
  needs nothing.
- **Signals**: a sandboxed process may signal only its own app's processes
  (`processes` lifts that), on top of the account rule.
- **Administrators**: a sandboxed app runs without the administrator
  capability and the admin group unless it declares `admin`; it may ask
  for it only with `elevate`.
- **Background**: an app without `background` is ended ten seconds after
  its last window closes, unless it opens another — and unless it is still
  at work: another process of the same app has a window (a launcher whose
  game shows one, a bridge whose client does), it has programs of its own
  running (it waits for them, or they are starting), or it holds a DRM
  card (full-screen programs such as `kmscube` never own a window). Only
  closing a window starts the clock, so programs that never had one are
  never ended; programs without a manifest and those that sandboxed
  themselves are exempt.
- **Input devices** (`compat/linux/input`, `drivers/evdev`): keyboard and
  pointer nodes open for anyone and deliver events only to the program
  whose window has the focus (or to a sandboxed app the person allowed
  `input-monitoring`); the rest is in the table above. Device discovery —
  `/sys`, `/run/udev` (never moved into the container), `/dev/input`
  listings, uevents, inotify — is everyone's, as on Linux.
- **Windows shown for another process** (`win_set_program`, used by
  `wl-bridge`): the window's focus gives that process the keyboard,
  pointer and controllers, so a sandboxed caller may name only its own
  app's processes (the signal rule; `processes` lifts it).
- **IPC**: Linux local sockets (paths and the abstract namespace) and
  `/dev/shm` objects of a sandboxed app without `ipc` live in a namespace
  of its own (`sandbox::ipc_key`): it reaches its own processes and nobody
  else, and nobody else reaches it. Descriptors it is sent (`SCM_RIGHTS`)
  and inherits work: a descriptor is a capability.

## Which rules a program gets

| Program | Profile | Why |
|---|---|---|
| Started by the kernel (boot, the self-tests) | none | the system's own |
| The system's own apps (`/System/Apps/*.elf`) | `System`: their `userland/apps/*/manifest.toml`, built into the kernel | explicit, reviewed lists; the folders they declare need no question, devices do |
| Installed apps, format-2 manifest | `Package`: exactly the declared permissions | the package format already carried them; the installer shows them |
| Installed apps, format-1 manifest | `Legacy`, under the app's id | they predate permissions and must keep working |
| Anything else the desktop starts (tools from Seek, Linux programs, Wave Play's Linux and Steam games) | `Legacy`, id `legacy.<name>` | no manifest to go by |
| Started by a sandboxed program | the parent's (or the other app's, with `launch`) | children only ever have less |
| A program that sandboxed itself | `Restricted`, id `local.<name>` | it keeps what it kept |

**The default profile (`Legacy`)** may do what its account may — every
file Unix permissions allow, the network, starting programs, other apps,
the administrator capability of an administrator account — except the
guarded devices (microphone, camera, screen, input, USB), which it must ask
for under its own name. Programs without a manifest cannot say what they
need, so taking anything else away would silently break them; the devices
are where the person's privacy is, and asking cannot break anything that
works today, only make it wait for an answer once.

**Terminal** and everything run in it: Terminal's manifest declares
`all-files` and every capability, with the devices guarded, so command-line
tools and Linux programs started there behave as before, and the first
recording asks "Allow Terminal to use the microphone?", as on macOS.

**Linux programs** honour their account's file permissions (as before) and
the grants of the app that started them: they are its children. A
sandboxed app's Linux programs find `/tmp`, `/var/tmp`, `/run` and
`/home/<user>` in its container (unless it has `linux-home`, as Firefox and
Voyage do, to keep their existing profiles), may not change the shared link
table (so cannot plant links for others), and need `linux` to start at all,
`spawn` to `execve`, `display` for the DRM card.

The system apps' manifests (in `userland/apps/<app>/manifest.toml`) give
each what it does: Files, Installer, Settings and Terminal `all-files`;
Notes, Paint, Preview, Studio, Firefox and Voyage `home`; Doom `documents`;
Clock its old alarm file (`/Settings/clock.conf`); Calculator nothing.

## Changes from the first design

The design note this replaces proposed a `sandbox: Option<Arc<Sandbox>>`
inside `users::Cred`. `Cred` is `Copy` and copied everywhere, so the sandbox
lives beside it on the process instead; children copy both. The capability
list grew (camera, screen, input, USB, launch, IPC, background, display,
system-only bits) and `AUDIO_IN`/`USER_FILES` became `MICROPHONE`/`HOME`.
Grants are path prefixes, as proposed (the open question of inode grants is
answered below). Linux programs did not get a coarser separate profile:
they take their launcher's, with private temporary and home folders.

## Threat model

The sandbox defends the person's files, devices and other apps against an
app that runs as them and is malicious or compromised: its code, any
Linux programs it starts, and anything it can make the system do for it.

In scope, and covered by the self-tests (below):

- Reading or changing files outside its container and grants through any
  file call, native or Linux, with `..`, relative paths, directory
  descriptors, `/proc/self/fd`, symbolic links (followed, then checked;
  planting new ones in the shared table is refused), hard links (none
  exist: `link` is `EPERM`), renames racing opens (checks are on the final
  path, and there is nothing in between to swap).
- Inherited and passed descriptors: they work (that is how a grant by a
  less confined process looks) but do not widen anything else — a
  directory descriptor is a path, checked again.
- Laundering: getting another app or the desktop to act for it — opening a
  document it may not read in another app, putting files on the clipboard
  or dragging them, passing paths to an app it launches, setting a
  wallpaper from a file. Each is checked against the requester.
- Devices and other apps: the network, the microphone (also after a
  revocation, mid-stream), sound, controllers, the DRM card, notifications,
  preferences, power, other apps' processes, sockets and shared memory, the
  administrator capability, the kernel log, Settings' own privacy calls.

Out of scope: the kernel itself (a kernel bug defeats every wall; the
fuzzers are there to find them), the person deliberately granting things,
other accounts (Unix permissions), physical access, and side channels
(timing, cache, power, GPU memory).

## Known gaps

- **Path grants follow paths, not files.** A granted file renamed or
  replaced keeps (or loses) access by its path; a bookmark to a moved file
  does not follow it. A folder grant covers whatever is later put there.
- **Open descriptors outlive revocation.** Switching off a folder stops new
  opens, not files already open (a recording, by contrast, stops at once).
- **The clipboard** is readable by whichever app is in front; reading it
  there counts as a paste, files and all.
- **`open_file`** gives the opening app the document to read and change
  even when the requester could only read it.
- **Loopback** is network: an app with `network` reaches any local server,
  including other apps' (TCP listeners need `network-server`).
- **Process listing** (`proc_list`) shows every process's name to anyone.
- **The camera and raw USB** have no interfaces yet; the bits are
  reserved and enforced wherever such an interface is added
  (`sandbox::require`). Screen capture guards the DRM card's foreign
  framebuffers; there is no screenshot call for apps yet. A DRM master
  that may drive the display (`display`) can still show what it likes.
- **Input monitoring is never asked about when reading** (the devices
  check it while locked): an app gets unfocused keyboard and pointer
  events only once the person allowed it — in Settings, or at its first
  grab or made keyboard. Device discovery (`/sys`, `/run/udev`) shows
  every app what devices exist.
- **Legacy programs** keep their account's rights (see above): install
  format-2 packages to get the sandbox's protection.
- **Root-level Spotlight** (the file index) is the window server's: it
  shows names of files to the person, not to apps.
- **Symbolic links** can't be made by a sandboxed Linux program at all
  (the table is shared).

## Testing

`cargo xtask test` runs (filter with `AURORA_TEST_FILTER=sandbox`):

- **sandbox: policy, consent, grants, bookmarks, system manifests** — the
  rules on their own, including revocation, the decisions on disk, the
  system apps' manifests and Settings' view.
- **sandbox: a native program … (sbxtest)** — `userland/bin/sbxtest` in a
  sandbox: everything it did not declare is refused (files, `..`, chmod,
  renames out and in, sockets, the microphone, sound, notifications,
  programs, controllers, preferences, the desktop, other apps' processes,
  the clipboard from behind, the administrator prompt, the kernel log,
  Settings' calls); an inherited descriptor works; `restrict` is for good.
  Then it is given files by the open panel, a drop and a paste, keeps a
  bookmark and uses it in a later run, and is refused a forged one. Last it
  is allowed what it declared, and Documents is switched off while it runs.
- **sandbox: a Linux program … (sandboxtest)** — `linux-tests/sandboxtest.c`
  tries paths, `..`, dirfds, `/proc`, symlinks (planting and following a
  planted one), hard links, renames racing opens, network sockets, another
  app's local socket and shared memory, its own socket with descriptor
  passing, signals, `execve`, the DRM card, sound and WaveOS calls.
- **sandbox: the native / Linux fuzzer never reaches the oracle** — both
  system-call fuzzers run in a sandbox with `--oracle /SandboxOracle`
  (`ORACLE` for `linuxfuzz`) and aim a quarter of their path arguments,
  mutating calls included, at it. `/SandboxOracle` is a filesystem of its
  own that counts every call a sandboxed process makes to it; the test
  fails on any count, on a changed secret or a changed folder.

Every test checks the oracle the same way.

- **sandbox: windowless apps end only when idle and without background** —
  an app without `background` that closes its last window is ended; with
  `background`, with a program of its own running, or never having had a
  window, it runs on.
- `sandboxtest` also covers input: `/sys`, `/run/udev` and `/dev/input` are
  visible; controllers' nodes, raw HID and making devices with `uinput`
  are refused; keyboards and pointers can neither be read nor grabbed
  without the focus; a window it shows may be said to show only its own
  process.
- The self-tests start every Linux test program (`linuxtest`, `proctest`,
  `nettest`, `inputtest`, `drmtest`, `modetest`, `linuxfuzz`) in the
  **default profile**, as the desktop would: since nobody answers prompts
  in the tests, anything there that needed one would fail. `sdlprobe` (the
  SDL controller check, needs the Debian packages) is not in the
  automated run; it uses controllers' nodes, udev and inotify, which the
  default profile and Terminal have without asking.
