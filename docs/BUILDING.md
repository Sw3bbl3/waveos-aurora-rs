# Building, running and debugging

For the versioned 0.7 user and developer guides, see the [bilingual handbook](handbook/README.md). This reference URL remains available.

## Requirements

| Tool | macOS | Debian / Ubuntu |
|---|---|---|
| Rust | [rustup](https://rustup.rs), with the exact Rust/LLVM baseline in `tools/toolchain-baseline.toml` | same |
| clang (for the Linux GPU drivers in `kernel/linux`; without it the kernel builds without them) | Xcode command-line tools, or `brew install llvm` | `sudo apt install clang` |
| musl-tools (for the Linux programs in the image, including Doom; without it they are left out) | not available; the image is built without them | `sudo apt install musl-tools` |
| QEMU + OVMF | `brew install qemu` (bundles `edk2-x86_64-code.fd`) | `sudo apt install qemu-system-x86 ovmf` |
| xorriso (ISO only) | `brew install xorriso` | `sudo apt install xorriso` |

`xtask` finds OVMF in the usual Homebrew and distro locations. To point it at a specific build, set `OVMF_CODE=/path/to/OVMF_CODE.fd`. It expects a matching `*VARS*` file in the same directory.

## The launcher: `make run`

```sh
make run          # or ./waveos: open the launcher page, choose, Launch
make quick        # or ./waveos quick: boot the configuration launched last, no page
make              # list every shortcut (build, test, lint, image, iso, clean)
```

`make run` runs `cargo xtask launch`. It serves a page on `127.0.0.1` (a random free port) and opens it in your default browser. The page has presets, every run option below with a short explanation, and the exact `cargo xtask run …` command for what you've chosen, with a copy button. The launcher runs precisely that command, so anything you launch from the page you can also type. **Launch** builds and boots, streaming the build output and the serial console into the page; **Stop** and **Restart** do what they say. If the build or QEMU fails, the page says so and keeps the output.

- **Presets.** Default, Low memory laptop (1 GiB, 2 CPUs, battery), Dual display, Real-hardware-like (NVMe, e1000e, xHCI), Headless and Kernel tests. **Save as preset** keeps your own, which you can rename and delete. They and the last configuration live in `.waveos-launcher.json` at the top of the repository, which git ignores and `make clean` keeps.
- **Options this QEMU can't do are greyed out.** The launcher asks `qemu-system-x86_64 -device help` and `-display help`. Homebrew's macOS QEMU, for example, has no `ivshmem`, so "This computer's battery" is off there.
- **A fresh disk is never a surprise.** The page asks before erasing `target/waveos-aurora.img`. The launcher refuses an unconfirmed fresh disk and never remembers one for `make quick`.
- **Lifetime.** Closing the page leaves WaveOS running. The launcher keeps going while WaveOS runs, and exits by itself once WaveOS has stopped and no page has been open for 30 seconds. **Ctrl-C** in the terminal stops everything: the launcher, any build and QEMU share the terminal's process group, so they all get the interrupt, and QEMU shuts down cleanly. **Stop** sends SIGTERM to the run and every process under it, and waits until QEMU has gone (SIGKILL after 10 seconds). **Quit launcher** on the page stops WaveOS and the launcher.
- **Options.** `cargo xtask launch --no-open` prints the URL without opening a browser; `--port N` picks the port.
- **Security.** The page is self-contained (no requests anywhere else). The server only listens on 127.0.0.1, checks the `Host` header, and only accepts changes posted as JSON from its own origin, so other web pages can't drive it.

## Several displays

```sh
cargo xtask run --displays 2                                   # two 1920×1080 displays
cargo xtask run --displays 3 --resolution 2560x1440           # three, all 2560×1440
cargo xtask run --resolution 1920x1080,1280x800                # one size per display (two displays)
cargo xtask run --displays 2 --gpu virtio                      # virtio-gpu with two outputs
```

`--displays N` (1 to 4) gives the virtual machine N monitors, each with its own EDID:
- `--gpu std` adds a `secondary-vga` per extra display, each preferring its own size from `--resolution`. Display 1's mode is chosen by WaveOS, as before.
- `--gpu virtio` uses one virtio-vga with `max_outputs=N`. The ported Linux driver logs `number of scanouts: N`. QEMU gives every output the same preferred size, display 1's.

Each display opens in its own host window. How depends on what your QEMU was built with:

| Host | Display 1 | Displays 2 to N |
|---|---|---|
| macOS (Homebrew QEMU: Cocoa, no SDL or GTK) | the QEMU window | one VNC server each on 127.0.0.1 (from port 5901), opened in **Screen Sharing** windows. Screen Sharing needs a VNC password, so each run makes a random one and passes it in the `vnc://` URL; the printed command hides it. |
| Linux with SDL | an SDL window | an SDL window each (`-display sdl`) |
| Linux with GTK only | the QEMU window | VNC, opened with `remote-viewer`, `vncviewer` or `gvncviewer` if installed; otherwise the address is printed |
| `--headless` | none | none; the guest still sees every display |

QEMU's Cocoa window can show only one display at a time (View menu), so VNC is what gives macOS separate windows. When QEMU exits, the Screen Sharing windows stay open with a "connection lost" message: close them yourself.

With `--gpu virtio` the desktop extends over every display: the menu bar and Harbor on display 1, the others to its right (arrange them in Settings → Display). QEMU switches outputs 2 to N on only when a user interface asks for a size, which Screen Sharing and most VNC viewers never do, so xtask asks itself through one more VNC server per output, on a Unix socket in the session directory. With `--gpu std` only display 1 is used (the `secondary-vga` windows keep showing the firmware's last text). One QEMU caveat: the USB tablet is absolute over the rectangle around all displays, so in a VM with several windows the pointer lands where that rectangle says rather than under the host cursor; use a mouse (`AURORA_QEMU_ARGS="-device usb-mouse"`) or move between displays at their shared edges.

`cargo xtask test --gpu virtio --displays 3` runs the multi-monitor kernel test: the harness plugs the extra heads in and out through their VNC servers and compares a QMP screendump of each head with what the compositor drew.

## Everyday commands

```sh
cargo xtask run                     # release build + boot in a QEMU window, 4 GiB RAM
cargo xtask run --memory-mib 8192    # override guest RAM (MiB)
cargo xtask run --monitor           # same, plus the live System Explorer at http://127.0.0.1:7777
cargo xtask run --disk nvme         # same, with the disk on NVMe (or: ahci, virtio)
cargo xtask run --fresh-disk        # start over with a new disk (erases files saved in WaveOS)
cargo xtask run --debug             # unoptimised kernel (slower, better backtraces)
cargo xtask run --headless          # no window; serial log on stdout (and no sound)
cargo xtask run --smp 1             # number of CPUs (default 4)
cargo xtask run --battery           # add a laptop battery (73 %, discharging), power adapter and lid (ACPI)
cargo xtask run --battery=host      # …that mirrors this computer's own battery, live (Linux, macOS, Windows hosts)
cargo xtask run --battery=20,charging  # …at a fixed level, charging or discharging
cargo xtask run --usb-stick         # plug in a 64 MiB FAT32 USB stick (target/usb-stick.img)
cargo xtask run --no-sound          # no host audio (sound goes nowhere)
cargo xtask run --headless --audio-wav target/playback.wav # record native guest audio
cargo xtask run --no-keyboard-capture # macOS: opt out of full keyboard capture
cargo xtask run --developer         # separate 8 GiB developer disk, 4 GiB RAM
cargo xtask run --net none          # disable networking for offline validation
cargo xtask run --gpu virtio        # virtio-vga display, driven by the native virtio-gpu driver
cargo xtask run --gpu virtio-linux  # the same display, driven by the ported Linux DRM virtio-gpu driver
cargo xtask run --wifi-sim          # simulated Wi-Fi networks ("Harbor Cafe", password "tide pools at dawn")
cargo xtask run --displays 2        # two simulated displays, each in its own window (see "Several displays")
cargo xtask launch                  # the launcher page (make run); --last boots the last configuration (make quick)
cargo xtask test [--disk virtio] [--smp 1] [--gpu virtio]   # kernel self-tests on a fresh disk, booted twice (exit status 0 = pass)
cargo xtask test --memory-mib 256        # the same on a small machine (default 512 MiB); AURORA_TEST_FILTER=swap for the swap tests only
cargo xtask lint                    # Clippy on every crate, for its own target; warnings are errors
cargo test -p aurora-help -p gina -p aurorafs -p fat32 -p lumen -p aurora-image -p aurora-wav -p aurora-wifi -p aurora-hid -p nebula-web -p nebula-secure -p nebula-engine -p nebula-script -p nebula-media -p aurora-elf -p aurora-mutate -p xtask   # host-side tests
AURORA_MUTATIONS=100 cargo test -p aurorafs --release   # robustness tests, 100x longer than CI runs them
cargo xtask image                   # target/waveos-aurora-usb.img for USB boot
cargo xtask iso                     # target/waveos-aurora.iso for UEFI virtual machines
cargo xtask iso --no-build          # package an existing target/esp without rebuilding
```

`cargo xtask run` boots `target/waveos-aurora.img`, a GPT disk of 256 MiB, or the size of the Linux packages plus 512 MiB (about 1.8 GiB) when they are on it:
- **Partition 1** is a FAT32 EFI System Partition with the bootloader, the kernel and `system.tar`. Each build rewrites it.
- **Partition 2** is **"Aurora HD"**, a AuroraFS volume that holds your files. It is created once from `assets/home/` and then kept, so notes, folders and settings survive rebuilds.

The virtual machine has 4 CPUs, Intel HD Audio (played through your Mac's speakers with Core Audio), a USB 3 controller with a tablet and, behind a hub, a keyboard, and S3 sleep enabled. On macOS, interactive launches request Cocoa full keyboard capture, forward Command, and disable Option/Command swapping. QEMU needs Accessibility permission; event-tap failures are reported on stderr. Click in the VM to capture and press **Control+Option+G** to release. Use `--no-keyboard-capture` to opt out. To wake WaveOS from sleep, press a key, or run `tools/qmp.py qmp:system_wakeup`.

The WaveSDK developer site is [docs/developer/index.html](developer/index.html). See [WaveSDK development](WAVESDK.md) for GINA packaging, native CLI commands, Studio, the compiler-port gap, developer disk sizing, and compatibility details.

`cargo xtask test` also attaches a USB stick, the test battery, and records the sound output to `target/test-audio.wav`. It plugs in virtual USB keyboards, touchpads and game controllers through QEMU's `usb-redir` when the local QEMU has it (it asks `-device help`); Homebrew's macOS QEMU doesn't, so those checks are skipped there with a warning. `AURORA_TEST_NO_USBREDIR=1` skips them anyway, and `=0` insists on them.

The launcher's own tests: `cargo test -p xtask` checks the option tables, the command lines and presets, and that `cargo xtask launch --no-open` serves the page. `cargo xtask build && cargo test -p xtask --test launcher -- --ignored` also launches a headless WaveOS through the launcher's API (on its own disk, `target/launcher-e2e.img`), waits for `Aurora desktop ready` and stops it. Its sleep test puts the VM to sleep and wakes it over QMP. Some OVMF builds (Ubuntu's, for one) fault while resuming, before WaveOS gets control: the harness notices and runs the suite again with `sleep=off`. Set `AURORA_TEST_SLEEP=off` to skip it from the start.

## Booting on real hardware (experimental)

`cargo xtask image` writes `target/waveos-aurora-usb.img`, a complete system to put on a USB stick. [HARDWARE.md](HARDWARE.md) explains how to write it, the boot options, what to check, and the known limits.

## VirtualBox ISO

`cargo xtask iso` builds `target/waveos-aurora.iso` with a UEFI El Torito boot image. In VirtualBox, create a 64-bit VM, enable **EFI** under System > Motherboard, attach the ISO to the optical drive, and boot it. Give the VM at least 2 GiB of RAM. The ISO is read-only, so files created in a session are kept in memory unless you also attach a WaveOS disk image with a WaveFS partition. This image boots via UEFI; legacy BIOS mode is unsupported. The disc's boot image holds at most 32 MiB, so the system image goes on it compressed and the bootloader unpacks it.

If VirtualBox says **No bootable medium found**, check that the VM firmware is set to EFI and that its optical drive points to the newly generated ISO. Changing an existing VM from BIOS to EFI may also require enabling 64-bit mode. Run `cargo xtask iso` again after code changes; an older copied ISO will not update automatically.

## Debugging

- **System Explorer.** `cargo xtask run --monitor` streams kernel telemetry to `docs/explorer/index.html`: boot stages, the CPU timeline, processes, memory, disk I/O and syscalls. The session is also recorded to `target/telemetry.jsonl`. To refresh the demo that plays when the page is opened on its own, regenerate `docs/explorer/demo.js` from that file.
- **Serial log.** Every subsystem logs to COM1, which `xtask` wires to your terminal. The kernel also keeps the last 64 KiB: `dmesg` in Terminal, and `\aurora\boot.log` on the EFI partition (written a few seconds after the desktop appears, with `lspci`, `lsusb` and `cpuinfo` reports).
- **Crash screen.** On a kernel panic or CPU exception, the kernel shows a crash screen with the details and also prints them to serial. Crashes in apps are contained: `crashtest` in Terminal, or `open /System/Bin/crashtest`, shows how.
- **Interrupt trace.** `cargo xtask run --int` writes QEMU's interrupt and CPU-reset trace to `target/qemu-int.log`. It's useful for triple faults.
- **GDB.** Run `cargo xtask run --gdb` to start QEMU paused with a GDB stub on `:1234`. Then:
  ```sh
  gdb target/x86_64-unknown-none/release/aster -ex 'target remote :1234'
  ```
- **Scripted control.** Scripts can drive the guest while it runs. `tools/qmp.py` wraps the common cases, for example `tools/qmp.py "click:553,752" "type:ls\n" "shot:out.png"`. The raw sockets are:
  - `target/qemu-monitor.sock` is the HMP monitor (for example `screendump shot.png -f png`).
  - `target/qemu-qmp.sock` is QMP (for example `input-send-event`).

Release boot images omit kernel DWARF to stay within the EFI optical-boot size limit. The full kernel ELF, including host debugger symbols, remains at `target/x86_64-unknown-none/release/aster`.
