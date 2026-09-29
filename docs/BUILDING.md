# Building, running and debugging

## Requirements

| Tool | macOS | Debian / Ubuntu |
|---|---|---|
| Rust | [rustup](https://rustup.rs), which installs the pinned nightly automatically | same |
| QEMU + OVMF | `brew install qemu` (bundles `edk2-x86_64-code.fd`) | `sudo apt install qemu-system-x86 ovmf` |

`xtask` finds OVMF in the usual Homebrew and distro locations. To point it at a specific build, set `OVMF_CODE=/path/to/OVMF_CODE.fd`. It expects a matching `*VARS*` file in the same directory.

## Everyday commands

```sh
cargo xtask run                     # release build + boot in a QEMU window
cargo xtask run --disk nvme         # same, with the disk on NVMe (or: ahci, virtio)
cargo xtask run --fresh-disk        # start over with a new disk (erases files saved in WaveOS)
cargo xtask run --debug             # unoptimised kernel (slower, better backtraces)
cargo xtask run --headless          # no window; serial log on stdout
cargo xtask test [--disk virtio]    # kernel self-tests on a fresh disk, booted twice (exit status 0 = pass)
cargo test -p wavefs -p fat32       # filesystem library tests on the host
cargo xtask image                   # target/waveos-aurora-usb.img for USB boot
```

`cargo xtask run` boots `target/waveos-aurora.img`, a 256 MiB GPT disk:
- **Partition 1** is a FAT32 EFI System Partition with the bootloader, the kernel and `system.tar`. Each build rewrites it.
- **Partition 2** is **"Aurora HD"**, a WaveFS volume that holds your files. It is created once from `assets/home/` and then kept, so notes, folders and settings survive rebuilds.

In a QEMU window with the absolute pointer, your mouse moves freely in and out of the guest. The keyboard follows focus.

## Booting on real hardware (experimental)

1. Run `cargo xtask image`.
2. Write the image to a USB stick. **This erases the stick.**
   - macOS: find the disk with `diskutil list`, then run:
     ```sh
     diskutil unmountDisk /dev/diskN
     sudo dd if=target/waveos-aurora-usb.img of=/dev/rdiskN bs=4m
     ```
   - Linux: `sudo dd if=target/waveos-aurora-usb.img of=/dev/sdX bs=4M conv=fsync`
3. Boot the PC from the stick in **UEFI mode**. Disable Secure Boot, because the loader is unsigned.

Current hardware limits:

- Input needs a PS/2 controller, or USB legacy emulation of one; native USB arrives in M5.
- Only one CPU core is used.
- The USB stick boots, but its storage isn't visible yet: USB mass storage arrives with xHCI in M5. Internal SATA (AHCI) and NVMe disks are detected; the OS uses a WaveFS partition if it finds one, and otherwise keeps your files in memory.

## Debugging

- **Serial log.** Every subsystem logs to COM1, which `xtask` wires to your terminal.
- **Crash screen.** On a kernel panic or CPU exception, the kernel shows a crash screen with the details and also prints them to serial. Crashes in apps are contained: `crashtest` in Terminal, or `open /System/Bin/crashtest`, shows how.
- **Interrupt trace.** `cargo xtask run --int` writes QEMU's interrupt and CPU-reset trace to `target/qemu-int.log`. It's useful for triple faults.
- **GDB.** Run `cargo xtask run --gdb` to start QEMU paused with a GDB stub on `:1234`. Then:
  ```sh
  gdb target/x86_64-unknown-none/release/tide -ex 'target remote :1234'
  ```
- **Scripted control.** Scripts can drive the guest while it runs. `tools/qmp.py` wraps the common cases, for example `tools/qmp.py "click:553,752" "type:ls\n" "shot:out.png"`. The raw sockets are:
  - `target/qemu-monitor.sock` is the HMP monitor (for example `screendump shot.png -f png`).
  - `target/qemu-qmp.sock` is QMP (for example `input-send-event`).
