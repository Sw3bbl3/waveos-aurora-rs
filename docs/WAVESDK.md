# WaveSDK development

For the versioned 0.7 user and developer guides, see the [bilingual handbook](handbook/README.md). This reference URL remains available.

The WaveSDK (formerly the Constellation SDK) contains the WaveUI interface framework, CoreKit, GINA installation, the `wavesdk` command and Studio. Every built-in app is written with WaveUI; see the handbook's [WaveUI guide](handbook/0.7/en/developer-waveui.md) and [Design guidelines](handbook/0.7/en/developer-design.md). **It does not yet meet the complete native compiler and debugger acceptance milestone.** Studio runs inside WaveOS, but Rust, Cargo, LLVM, rustfmt and rust-analyzer have not been ported to run there. Build reports that prerequisite explicitly; no host helper or simulated build is used.

## Component names and compatibility

| Component | Name / source |
|---|---|
| Desktop | WaveOS Aurora / Aurora |
| Kernel | Aster (`kernel`) |
| Compositor / graphics | Lumen Server / Lumen (`libs/gfx`) |
| Font engine | Lyra (Lumen's TrueType implementation) |
| UI / runtime | WaveUI / AuroraKit / CoreKit (`userland/waveui`, `userland/aurorakit`, `userland/corekit`) |
| Boot / filesystem | FirstLight / AuroraFS |
| Browser / rendering | NetSurf (`/System/NetSurf`) / Nebula Engine |
| HTTP / TLS / network | Nebula Web / Nebula Secure / Nebula Net |
| Developer tools | WaveSDK (formerly Constellation SDK) / Studio (`userland/wavesdk`, `userland/bin/wavesdk`) |
| External apps | GINA — General Interface for Native Applications |

The AuroraFS on-disk `WAVEFS01` signature, partition GUID, firmware paths, existing settings keys and user files retain their original formats. The regular desktop disk is preserved. An unsupported disk layout or failed ESP refresh now stops the launch instead of recreating the disk.

## WaveUI

WaveUI (`userland/waveui`) is a declarative framework in the style of SwiftUI and Elm. An app implements `waveui::App`: its state, a `Message` type, `update(message, &mut Context)` and `view(&Env) -> View`. The view tree is rebuilt after every update; WaveUI measures and places it (with a per-frame cache), draws it with Lumen in the current appearance and accent, and routes the pointer, keyboard, focus, scrolling, context and pull-down menus, sheets, tooltips, drag and drop and canvas input. Runtime state (focus, scroll offsets, field contents) is keyed by the view's path or its `.id()`. WaveUI runs as an `aurorakit::App`, so the AuroraKit drawing and event APIs remain available underneath.

## Desktop and AuroraKit

Lumen materials sample the scene already drawn beneath the surface, reuse downsampled blur and finished-material buffers, and invalidate when the backdrop changes. Moving windows use coarser sampling and restore stationary quality on release. Cursor-only frames reuse the completed scene. For correctness, other scene damage currently rebuilds the complete scene; fine-grained blur dependency regions remain optimization work.

Settings includes search, Desktop & Dock, and Accessibility pages. Glass intensity, dock size/auto-hide, Reduce Transparency, Reduce Motion, and Increase Contrast persist and apply live. Settings, like every built-in app, is written with WaveUI. Windows reserve a 34-point compositor control strip above the client content, with animated hover controls and keyboard access via Alt+Space. All edges and corners share resize hit-testing and directional pointers. Client content sizes, drawing coordinates, and input coordinates exclude the strip. Windows also support left/right snapping. Control Center shares the preference service with Settings.

WaveUI provides text, images and system symbols, buttons, toggles, sliders, steppers, pickers, text fields and editors, stacks, grids, lazy lists, scroll views, split views, sections, toolbars, menus, alerts, sheets and canvases. App glass regions opt in through a validated compositor API; existing apps remain opaque by default.

## Game controllers and Wave Play

`corekit::gamepad` lists controllers and reads them in one standard layout (face buttons by position, sticks −32768…32767 with right and down positive, triggers 0…65535), with rumble where the controller has motors. Linux programs use the same calls through `waveos.h` (`wave_gamepads`, `wave_gamepad_state`, `wave_gamepad_rumble`). A program reads a neutral controller (nothing pressed) while it isn't the one in front, or while Wave Play's own screens have the controllers, so a game never acts on presses meant for a menu. Programs that don't read controllers still work in Wave Play: the d-pad, A and B arrive as arrow keys, Enter and Escape.

Wave Play's library gathers games and apps from the desktop's catalog, installed GINA apps, Linux packages whose `.desktop` entry lists the `Game` category, and Steam's library when it is installed. A package manager or store adds tiles by writing one file per entry to `/Settings/Play/Library/<name>.play`:

```text
id=org.example.game
title=Example Game
kind=game
exec=/Linux/usr/games/example
args=--fullscreen
art=/Linux/usr/share/example/cover.png
detail=Installed with the package manager
```

`kind` is `game`, `app` or `setting`; `art` is an optional PNG, JPEG, GIF or BMP (cover art fills the tile); `exec` must be an absolute path. Without a controller, `cargo xtask run --virtual-gamepad` (or Super+G) connects one played on the keyboard, through the same code as a USB controller; Super+Shift+G slows Wave Play's motion eightfold for inspection, and `tools/smoke_play.py` drives the whole flow over QMP and collects screenshots and frame timings.

## GINA packages

An app reaches other people as a GINA package (`.gina`): a restricted, uncompressed ustar archive that holds a manifest, the app's files, a list of their hashes and, usually, its publisher's signature. `libs/gina` reads, writes and checks them (host and device share it); `corekit::apps` installs them; `pkg`, Installer and Settings → Apps are the front ends.

### The manifest (format 2)

The root `manifest.toml` uses a small subset of TOML (flat strings, integers and string arrays):

```toml
format = 2
id = "dev.example.gallery"          # lowercase reverse-DNS, unique per app
name = "WaveUI Gallery"
author = "Example Developer"        # the publisher shown to people
version = "1.2.0"                   # 1-4 numbers, optional -prerelease (1.2.0-beta.1)
description = "Every WaveUI control, in a few lines each."
architecture = "x86_64"
sdk = "0.9"
abi = 1
entry = "bin/app"                   # the program the launcher starts
icon = "Resources/icon.png"         # PNG, at most 1024x1024 and 1 MiB
resources = ["Resources/help.txt"]  # files that must be present
extensions = ["gallery"]            # file associations: Files offers "Open with" for these
categories = ["developer"]          # game, productivity, utility, developer, graphics,
                                    # media, education, network, social, system (up to 4)
permissions = ["network", "documents:read"]
min_os = "0.8"                      # the oldest WaveOS it runs on ("" = any)
dependencies = ["dev.example.runtime >= 1.0"]   # id, "id >= v" or "id = v"
```

Versions are ordered numerically (`1.10` > `1.9`; `1.2` = `1.2.0`; `2.0.0-rc.1` < `2.0.0`). Unknown keys are refused, so a package can't declare itself a built-in or privileged app.

**Permissions** are shown before installing and **enforced by the kernel** for format-2 packages: the app runs in a sandbox ([developer/sandboxing.md](developer/sandboxing.md)) and may use only what it declares — `network` (NET_CLIENT), `network-server` (NET_SERVER), `audio` (AUDIO_OUT), `microphone` (MICROPHONE), `camera`, `screen-capture`, `input-monitoring`, `usb`, `gamepad` (GAMEPADS), `clipboard` (CLIPBOARD_READ, reading it while another app is in front), `notifications` (NOTIFY), `background` (running without a window), `preferences` (PREFS_WRITE), `spawn` (SPAWN), `launch` (opening other apps and documents), `ipc` (other apps' local sockets and shared memory), `linux` (LINUX), `display` (the DRM card), `elevate` (ELEVATE), `home` (HOME: all your files), and folders `documents`, `pictures`, `downloads`, `music`, `movies`, `desktop` (read and write) or with `:read`. The microphone, camera, screen, input, USB and folders are asked about the first time the app uses them; the person can switch any of them off in Settings → Privacy & Security. Format-1 packages, which predate permissions, run with their account's rights (the devices still ask).

Every app may always use its own **container**, `~/Library/Containers/<id>` (`Documents/`, `Preferences/`, `Caches/`, `tmp/`, `Home/`), without hard-coded paths: `corekit::sandbox::{container, documents, preferences, preferences_file, caches, temporary}` or `aurorakit::container`. Files beyond it come from the person: `corekit::sandbox::open_panel()`, `save_panel(name)` and `choose_folder()` show the system panel and give the app exactly what was picked; files dropped on the app (`drag_data`), pasted into it or opened with it are its to use too; `sandbox::bookmark(path)` returns a token to keep in the container, and `sandbox::resolve(token)` brings the file back in a later run. `sandbox::restrict(keep)` gives capabilities up for good, and `desktop::open_app_with(app, args)` opens another app as itself (with `launch`), passing it the documents among `args` this app may use.

Format 1 manifests (`format = 1`, `developer` instead of `author`, none of the format 2 keys) still install; they can't be signed and have no hash list.

### Inside the archive

```
manifest.toml
META/hashes       one line per payload file, sorted: <sha256> <size> <path>
META/signature    ed25519 <public key hex> <signature hex>   (optional)
bin/app, Resources/…   the payload
```

The signature is Ed25519 (RustCrypto's `ed25519-dalek`, strict verification) over `"GINA-2 package signature\0" ‖ SHA-256(manifest.toml) ‖ SHA-256(META/hashes)`, so it covers the manifest and, through the hash list, every payload byte. The parser refuses: anything but regular files and folders (symbolic and hard links, devices, GNU/pax headers); absolute paths, `..`, `.`, empty components, backslashes, colons and control characters; names that differ only in case; more than 1024 entries, 16 levels or 64 MiB; sizes that run past the archive; bad header checksums; payload files missing from, extra to or different from the hash list; other files under `META/`; a manifest whose entry, icon or resources are missing; an entry that is not a WaveOS x86-64 executable. A signature that doesn't verify is an error, not "unsigned". The parsers are fuzzed (`cargo test -p gina`, which also runs thousands of mutated packages, manifests and indexes).

### Build, sign and publish

1. **Make a publisher key** (once; keep the secret file private and backed up):
   ```sh
   cargo xtask keygen ~/keys/example.key "Example Developer"
   # writes example.key (secret) and example.key.pub (share this)
   ```
2. **Build the app** for `x86_64-aurora-user` (any program in `userland/apps` or a WaveSDK project).
3. **Package and sign it:**
   ```sh
   cargo xtask package path/to/manifest.toml target/user/x86_64-aurora-user/release/app-myapp \
       dist/dev.example.myapp-1.2.0.gina --sign ~/keys/example.key
   ```
   The icon and resources are read relative to the manifest. Without `--sign` the package is unsigned: it installs only after the person accepts a warning.
4. **Publish a repository:** put the packages in a folder (optionally with Store listings: `<app id>/listing.txt` and `<app id>/screenshots/*.png`, see [developer/store-and-updates.md](developer/store-and-updates.md)) and run
   ```sh
   cargo xtask repo dist --sign ~/keys/example.key --out site/stable
   ```
   which copies them to `packages/`, their icons and screenshots to `assets/`, and writes the signed catalogue (`catalog.txt`, `catalog.sig`) and, for older systems, `index.txt` and `index.sig`. Serve the folder over HTTPS (HTTP works too: everything is signed). Each catalogue has a version one higher than the last and expires after 30 days (`--expires-days N`): devices refuse an older catalogue from the same address (rollback) and an expired one (freeze), so re-run it at least that often. Packages are checked against the size and SHA-256 the catalogue names, and then their own signature is checked again. Trust roots (`root/<n>.txt`) let a repository move to a new signing key; `--mirror URL` lists mirrors.
5. **Users add your key and repository** (both need an administrator):
   ```sh
   pkg trust example.key.pub
   pkg source add https://example.dev/stable/
   pkg search myapp
   pkg install dev.example.myapp
   ```

Each build of WaveOS makes its own sample key (`target/keys/waveos-samples.key`, never committed) and trusts it (`/System/Keys/waveos-samples.pub`); `cargo xtask build` writes a demo repository to `target/repo` (WaveUI Gallery 1.0.0 and 1.1.0 and Doom as a `game`, with their Store pages, and a WaveOS update to the next version signed with the build's system update key), signed with it. Serve it with `cargo xtask serve-repo` and add `http://10.0.2.2:8000/` in the Store (or `pkg source add`). WaveOS itself comes with no repository configured.

### Installing

| Where | What |
|---|---|
| `/Applications/<id>/` | apps for everyone: the `admin` group's; others are asked for an administrator's password ([`authorize`](../userland/corekit/src/apps/trust.rs)) |
| `~/Library/Applications/<id>/` | apps for one account (`pkg install --user`, "Install only for me") |
| `<root>/<id>/active` | the live generation's folder name |
| `<root>/<id>/<version>-<sha256>/` | a generation: the package's files, `META/hashes`, `META/signature`, and `META/receipt` (when, from where, signed by whom, size) |
| `<root>/<id>/previous` | the generation an update replaced, kept for `pkg rollback` and the Store's Revert |
| `<root>/.registry` | every installed app, one tab-separated line each: id, version, name, categories, program, icon, folder, extensions, size, publisher (for programs such as Wave Play; `corekit::apps::in_category("game")` reads the same) |
| `~/Library/Containers/<id>/` | the app's data, kept apart from its files |
| `/System/Keys/*.pub`, `/Applications/.keys/*.pub` | trusted publisher keys (shipped; added by administrators) |
| `/Applications/.sources` | repository addresses (none out of the box) |
| `~/Library/Packages/` | per account: the newest catalogue version seen per repository, trust roots, resumable downloads |

An install stages the new generation in `.pending-<pid>`, flushes it, renames it into place, then writes `active` the same way (new file, flush, rename). Interrupted anywhere, the old version or the new one is live, never a mix; leftovers are cleared by the next install. Under `/Applications` files belong to `system:admin` (programs `rwxr-xr-x`, other files `rw-r--r--`, folders `rwxrwxr-x`) and the registry is `rw-r--r-- system`, so other accounts can run apps but not change them. Updates keep the app's data, must come from the same publisher key as the installed version, and may not go to an older version without `--allow-downgrade`. Missing dependencies and a too-old system stop an install before anything is written.

Installed apps appear in Shoal (games also under Play), Seek and Settings → Apps with their own icons as soon as they're installed, removed the same way; `pkg pin ID` or the *In Dock* switch in Settings → Apps keeps one in the dock (the `dock_apps` preference), and running ones show their icon there. A program that is a Linux executable must say so (an interpreter, a GNU note or ELF OSABI 3); `xtask/src/package.rs` marks Doom's game binary that way.

```sh
pkg install FILE.gina | URL | ID [--user] [--version V] [--allow-untrusted] [--allow-downgrade]
pkg remove ID [--delete-data]       pkg list [--category game]     pkg info ID|FILE
pkg verify [ID]                     pkg update [ID] [--check]      pkg search [TERM]
pkg rollback ID                 pkg repo [URL]
pkg system [status|check|download|install|restart|discard] [--version V]
pkg source [list|add URL|remove URL]   pkg trust FILE.pub | untrust KEY | keys
pkg pin ID | unpin ID               pkg registry
```

Opening a `.gina` file in Files starts Installer: name, version, publisher, signature status, requested permissions, size and category, then progress. Unsigned packages and keys you don't trust need the "I trust where this package came from" box ticked first.

For a precompiled example, open `/System/Developer/Examples/WaveUIGallery.gina`. It is compiled by the host when assembling the system image; it is **not evidence of native compilation**. The Store and Software Update are described in [developer/store-and-updates.md](developer/store-and-updates.md).

## Native commands and Studio

Run these commands in WaveOS Terminal:

```sh
wavesdk doctor
wavesdk new /Documents/Projects/MyApp dev.example.myapp "My App"
wavesdk generate /Documents/Projects/MyApp
wavesdk inspect /System/Developer/Examples/WaveUIGallery.gina
wavesdk install /System/Developer/Examples/WaveUIGallery.gina
wavesdk list
wavesdk launch dev.example.gallery
```

`constellation` remains an alias of `wavesdk`. `build` and `run` require the unported native tools in `/System/Developer/bin`. `package PROJECT` can export a validated package when an actual executable exists at `PROJECT/build/app.elf`. The host command `cargo xtask package MANIFEST APP.elf OUTPUT.gina [--sign KEY]` uses the same validator.

Studio provides project creation/opening, file navigation, Rust lexical highlighting, an editor, unsaved-change handling, a simple visual component palette/preview/property inspector, separate generated WaveUI source, background operation output, export and installation. Visual files live in `layouts/main.ui`; generated code lives in `src/generated_ui.rs`; handwritten `src/main.rs` is not regenerated. The first layout format is a flat column of `heading`, `label`, `button` and `toggle` lines.

Missing IDE features include semantic completion/navigation/refactoring, full project search, clickable diagnostics, advanced layout hierarchy, theme/resolution preview controls, and source debugging. Process launch/log/stop plumbing is present, but debugger attachment, breakpoints, stepping, DWARF inspection and crash stack mapping are not implemented.

## Toolchain port prerequisites

`tools/toolchain-baseline.toml` records the exact Rust commit and LLVM version. `xtask build` rejects a different compiler/LLVM pair. `rust-toolchain.toml` selects nightly; install the matching snapshot before a fresh build, or perform an intentional baseline migration. The SDK source bundle is not yet a complete offline sysroot: registry dependencies and standard libraries still need bundling.

The native port still needs a WaveOS host target, standard-library OS interfaces, Cargo-compatible process/environment semantics, thread-local storage, toolchain-compatible waiting and memory-protection facilities, required dynamic loading, LLVM/linker integration, Cargo build-script/procedural-macro execution, rust-analyzer and a controlled process-debug ABI. CoreKit already supplies basic arguments, cwd, files, threads, synchronization and pipes. The final offline Studio create → build → install → run → debug → export → reboot test must wait for working native tools.

Use `cargo xtask run --developer` for a **separate** persistent developer disk (`target/waveos-developer.img`, 8 GiB default) and 4 GiB RAM. Override with `--memory-mib N` and `--disk-size-mib N`. Existing disks are never resized by these options. These defaults reserve room for development but are not measured toolchain storage requirements; the port must be sized before it is bundled.

## macOS keyboard capture

Interactive macOS launches request `cocoa,full-grab=on,left-command-key=on,swap-opt-cmd=off`. Headless and Linux routing are retained. Use `--no-keyboard-capture` to disable full capture. QEMU needs macOS Accessibility permission for its event tap; capture errors remain visible on stderr. Click inside QEMU to capture, and press **Control+Option+G** to release.

Grant permission in **System Settings → Privacy & Security → Accessibility** when macOS requests it, then restart QEMU. The launch flags alone do not prove host shortcuts are captured: verify Command+Space and Command+Tab through actual macOS input. QMP-generated guest keys bypass that host path. See the [QEMU Cocoa display documentation](https://qemu-project.gitlab.io/qemu/system/invocation.html).

## UI assets and introduction

`waveui::cursor` exposes additive pointer shapes and `.cursor(shape)` overrides feedback for a view. The runtime selects text, link, and drag shapes automatically and AuroraKit sends changed shapes through `WIN_SET_CURSOR`. `Request::StartTour` and `corekit::process::desktop::start_tour()` request the optional shell-owned introduction (desktop request 15; existing requests retain their values).

Use `corekit::time::uptime_ms()` for durations and `unix_ms()` for UTC timestamps. Additive syscall `UNIX_MS` (83) supplies the corrected wall clock; changing the presentation UTC offset preserves the instant used by TLS and cookies. Existing syscall numbers remain unchanged. Linux C bindings expose the new clock and cursor values in `libs/linux/include/waveos.h`. Native network caches follow interface/address/router/resolver changes; `corekit::diagnostics::network(url)` performs an explicit layered check and does not start background Internet requests.

The photographic masters and provenance are in `assets/wallpaper-sources`. Run `python tools/prepare_wallpapers.py` with Pillow installed to reproduce offline runtime images and thumbnails. Build does not download media. App icon SVG sources are in `assets/icon-sources`; `tools/design_icons.py` reproduces them, and resvg-py can render the committed PNGs.

`App::min_size()` declares the minimum useful content size. AuroraKit advertises `win::MINIMUM_SIZE` in `WIN_CREATE` and packs width/height in its sixth argument. The compositor adds its own 34-point strip. Existing callers without the flag retain the legacy minimum and their sixth argument is ignored.
