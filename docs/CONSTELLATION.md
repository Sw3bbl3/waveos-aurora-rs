# Aurora development milestone

This branch adds Lumen backdrop materials, AuroraKit declarative controls, GINA installation, and the first native Constellation Studio application. **It does not yet meet the complete native compiler and debugger acceptance milestone.** Studio runs inside WaveOS, but Rust, Cargo, LLVM, rustfmt and rust-analyzer have not been ported to run there. Build reports that prerequisite explicitly; no host helper or simulated build is used.

## Component names and compatibility

| Component | Name / source |
|---|---|
| Desktop | WaveOS Aurora / Aurora |
| Kernel | Aster (`kernel`) |
| Compositor / graphics | Lumen Server / Lumen (`libs/gfx`) |
| Font engine | Lyra (Lumen's TrueType implementation) |
| UI / runtime | AuroraKit / CoreKit (`userland/aurorakit`, `userland/corekit`) |
| Boot / filesystem | FirstLight / AuroraFS |
| Browser / rendering | Nebula / Nebula Engine |
| HTTP / TLS / network | Nebula Web / Nebula Secure / Nebula Net |
| Developer tools | Constellation SDK / Constellation Studio |
| External apps | GINA — General Interface for Native Applications |

The AuroraFS on-disk `WAVEFS01` signature, partition GUID, firmware paths, existing settings keys and user files retain their original formats. The regular desktop disk is preserved. An unsupported disk layout or failed ESP refresh now stops the launch instead of recreating the disk.

## Desktop and AuroraKit

Lumen materials sample the scene already drawn beneath the surface, reuse downsampled blur and finished-material buffers, and invalidate when the backdrop changes. Moving windows use coarser sampling and restore stationary quality on release. Cursor-only frames reuse the completed scene. For correctness, other scene damage currently rebuilds the complete scene; fine-grained blur dependency regions remain optimization work.

Settings includes search, Desktop & Dock, and Accessibility pages. Glass intensity, dock size/auto-hide, Reduce Transparency, Reduce Motion, and Increase Contrast persist and apply live. The new pages use `aurorakit::ui::{View, State, Action}`. Existing drawing and event APIs remain available. Windows support edge/corner resizing and left/right snapping. Control Center shares the preference service with Settings.

The initial declarative controls include labels, headings, buttons, toggles, sliders, rows, columns, grids, scrolling, cards and glass panels. Existing text fields/editors remain separate controls. Full declarative lists, menus, dialogs, validation and task integration are not complete. App glass regions opt in through a validated compositor API; existing apps remain opaque by default.

## GINA packages

GINA uses restricted, uncompressed ustar archives. The root `manifest.toml` uses this supported subset of TOML (flat strings, integers and string arrays):

```toml
format = 1
id = "dev.example.gallery"
name = "AuroraKit Gallery"
developer = "Example Developer"
version = "1.0.0"
architecture = "x86_64"
sdk = "0.6"
abi = 1
entry = "bin/app"
icon = ""
resources = []
extensions = []
```

The shared validator checks archive checksums, bounded sizes, duplicate paths, path traversal, unsupported links, required resources, ABI/architecture and executable load bounds. It rejects unknown manifest keys, including attempts to designate a package as a built-in app. A package is limited to 64 MiB and 1024 entries.

Payloads are staged under `/Applications/<app-id>/`, then selected by an atomically replaced `active` registration only after files are written and synced. Generations use the version and archive SHA-256. App data stays in `/AppData/<app-id>/`. Installation does not overwrite the active generation. Uninstall retains app data unless `--delete-data` or the installer deletion option is selected. This is a single-user prototype; package signatures, permissions and isolation between app data directories are future work.

Open `.gina` in Files for install/update details, progress and launch. Installed apps appear in Launcher and desktop search, can run independently of Studio, and participate in file associations. Third-party dock icons are currently generic.

For a precompiled installation exercise, open `/System/Developer/Examples/AuroraKitGallery.gina`. This example is compiled by the host when assembling the system image; it is **not evidence of native compilation**.

## Native commands and Studio

Run these commands in WaveOS Terminal:

```sh
constellation doctor
constellation new /Documents/Projects/MyApp dev.example.myapp "My App"
constellation generate /Documents/Projects/MyApp
constellation inspect /System/Developer/Examples/AuroraKitGallery.gina
constellation install /System/Developer/Examples/AuroraKitGallery.gina
constellation list
constellation launch dev.example.gallery
```

`build` and `run` require the unported native tools in `/System/Developer/bin`. `package PROJECT` can export a validated package when an actual executable exists at `PROJECT/build/app.elf`. The host command `cargo xtask package MANIFEST APP.elf OUTPUT.gina` uses the same validator.

Studio provides project creation/opening, file navigation, Rust lexical highlighting, an editor, unsaved-change handling, a simple visual component palette/preview/property inspector, separate generated Rust source, background operation output, export and installation. Visual files live in `layouts/main.ui`; generated code lives in `src/generated_ui.rs`; handwritten `src/main.rs` is not regenerated. The first layout format is a flat column of `heading`, `label`, `button` and `toggle` lines.

Missing IDE features include semantic completion/navigation/refactoring, full project search, clickable diagnostics, advanced layout hierarchy, theme/resolution preview controls, and source debugging. Process launch/log/stop plumbing is present, but debugger attachment, breakpoints, stepping, DWARF inspection and crash stack mapping are not implemented.

## Toolchain port prerequisites

`tools/toolchain-baseline.toml` records the exact Rust commit and LLVM version. `xtask build` rejects a different compiler/LLVM pair. `rust-toolchain.toml` selects nightly; install the matching snapshot before a fresh build, or perform an intentional baseline migration. The SDK source bundle is not yet a complete offline sysroot: registry dependencies and standard libraries still need bundling.

The native port still needs a WaveOS host target, standard-library OS interfaces, Cargo-compatible process/environment semantics, thread-local storage, toolchain-compatible waiting and memory-protection facilities, required dynamic loading, LLVM/linker integration, Cargo build-script/procedural-macro execution, rust-analyzer and a controlled process-debug ABI. CoreKit already supplies basic arguments, cwd, files, threads, synchronization and pipes. The final offline Studio create → build → install → run → debug → export → reboot test must wait for working native tools.

Use `cargo xtask run --developer` for a **separate** persistent developer disk (`target/waveos-developer.img`, 8 GiB default) and 4 GiB RAM. Override with `--memory-mib N` and `--disk-size-mib N`. Existing disks are never resized by these options. These defaults reserve room for development but are not measured toolchain storage requirements; the port must be sized before it is bundled.

## macOS keyboard capture

Interactive macOS launches request `cocoa,full-grab=on,left-command-key=on,swap-opt-cmd=off`. Headless and Linux routing are retained. Use `--no-keyboard-capture` to disable full capture. QEMU needs macOS Accessibility permission for its event tap; capture errors remain visible on stderr. Click inside QEMU to capture, and press **Control+Option+G** to release.

Grant permission in **System Settings → Privacy & Security → Accessibility** when macOS requests it, then restart QEMU. The launch flags alone do not prove host shortcuts are captured: verify Command+Space and Command+Tab through actual macOS input. QMP-generated guest keys bypass that host path. See the [QEMU Cocoa display documentation](https://qemu-project.gitlab.io/qemu/system/invocation.html).
