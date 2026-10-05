# Building, running and debugging

## Requirements

| Tool | macOS | Debian / Ubuntu |
|---|---|---|
| Rust | [rustup](https://rustup.rs), with the exact Rust/LLVM baseline in `tools/toolchain-baseline.toml` | same |
| QEMU + OVMF | `brew install qemu` (bundles `edk2-x86_64-code.fd`) | `sudo apt install qemu-system-x86 ovmf` |
| xorriso (ISO only) | `brew install xorriso` | `sudo apt install xorriso` |

`xtask` finds OVMF in the usual Homebrew and distro locations. To point it at a specific build, set `OVMF_CODE=/path/to/OVMF_CODE.fd`. It expects a matching `*VARS*` file in the same directory.

## Everyday commands

```sh
cargo xtask run                     # release build + boot in a QEMU window
cargo xtask run --monitor           # same, plus the live System Explorer at http://127.0.0.1:7777
cargo xtask run --disk nvme         # same, with the disk on NVMe (or: ahci, virtio)
cargo xtask run --fresh-disk        # start over with a new disk (erases files saved in WaveOS)
cargo xtask run --debug             # unoptimised kernel (slower, better backtraces)
cargo xtask run --headless          # no window; serial log on stdout (and no sound)
cargo xtask run --smp 1             # number of CPUs (default 4)
cargo xtask run --battery           # add a test laptop battery, power adapter and lid (ACPI)
cargo xtask run --usb-stick         # plug in a 64 MiB FAT32 USB stick (target/usb-stick.img)
cargo xtask run --no-sound          # no host audio (sound goes nowhere)
cargo xtask run --no-keyboard-capture # macOS: opt out of full keyboard capture
cargo xtask run --developer         # separate 8 GiB developer disk, 4 GiB RAM
cargo xtask run --net none          # disable networking for offline validation
cargo xtask test [--disk virtio] [--smp 1]   # kernel self-tests on a fresh disk, booted twice (exit status 0 = pass)
cargo test -p aurorafs -p fat32 -p lumen -p gina -p aurora-image -p aurora-wav -p xtask   # host-side tests
cargo xtask image                   # target/waveos-aurora-usb.img for USB boot
cargo xtask iso                     # target/waveos-aurora.iso for UEFI virtual machines
cargo xtask iso --no-build          # package an existing target/esp without rebuilding
```

`cargo xtask run` boots `target/waveos-aurora.img`, a 256 MiB GPT disk:
- **Partition 1** is a FAT32 EFI System Partition with the bootloader, the kernel and `system.tar`. Each build rewrites it.
- **Partition 2** is **"Aurora HD"**, a AuroraFS volume that holds your files. It is created once from `assets/home/` and then kept, so notes, folders and settings survive rebuilds.

The virtual machine has 4 CPUs, Intel HD Audio (played through your Mac's speakers with Core Audio), a USB 3 controller with a tablet and, behind a hub, a keyboard, and S3 sleep enabled. On macOS, interactive launches request Cocoa full keyboard capture, forward Command, and disable Option/Command swapping. QEMU needs Accessibility permission; event-tap failures are reported on stderr. Click in the VM to capture and press **Control+Option+G** to release. Use `--no-keyboard-capture` to opt out. To wake WaveOS from sleep, press a key, or run `tools/qmp.py qmp:system_wakeup`.

See [Constellation development](CONSTELLATION.md) for GINA packaging, native CLI commands, Studio, the compiler-port gap, developer disk sizing, and compatibility details.

`cargo xtask test` also attaches a USB stick, the test battery, and records the sound output to `target/test-audio.wav`. Its sleep test puts the VM to sleep and wakes it over QMP. Some OVMF builds (Ubuntu's, for one) fault while resuming, before WaveOS gets control: the harness notices and runs the suite again with `sleep=off`. Set `AURORA_TEST_SLEEP=off` to skip it from the start.

## Booting on real hardware (experimental)

`cargo xtask image` writes `target/waveos-aurora-usb.img`, a complete system to put on a USB stick. [HARDWARE.md](HARDWARE.md) explains how to write it, the boot options, what to check, and the known limits.

## VirtualBox ISO

`cargo xtask iso` builds `target/waveos-aurora.iso` with a UEFI El Torito boot image. In VirtualBox, create a 64-bit VM, enable **EFI** under System > Motherboard, attach the ISO to the optical drive, and boot it. Give the VM at least 2 GiB of RAM. The ISO is read-only, so files created in a session are kept in memory unless you also attach a WaveOS disk image with a WaveFS partition. This image boots via UEFI; legacy BIOS mode is unsupported.

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
