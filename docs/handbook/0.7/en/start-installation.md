# Installation and updating
Use an x86-64 UEFI virtual machine. Apple Silicon hosts run the x86-64 guest through QEMU emulation; WaveOS does not boot natively on Apple Silicon. Legacy BIOS is unsupported.

## Before you begin
Download the 0.7.0 release image and SHA256SUMS. Verify the download with `shasum -a 256 FILE` on macOS or `sha256sum FILE` on Linux. Keep a copy of any existing WaveOS disk before updating.

## Run from source
1. Install Rust with rustup and QEMU with matching OVMF firmware. On macOS use `brew install qemu`; on Debian/Ubuntu use `sudo apt install qemu-system-x86 ovmf`.
2. Check out the release source. The dated Rust toolchain installs through rustup.
3. Run `cargo xtask run`. The default VM uses four CPUs and 512 MiB RAM.
4. Save a file in Documents, restart WaveOS from its menu, and confirm that the file remains.

The launcher refreshes the system partition while preserving the home volume. `--fresh-disk` erases the selected disk; use it only for disposable test images.

## ISO and USB
1. For an optical VM, attach the ISO and enable UEFI. Allocate at least 2 GiB RAM when following the VirtualBox configuration.
2. An ISO session is read-only at the source. Home files are temporary unless a compatible writable AuroraFS volume is attached.
3. For USB boot, write the decompressed IMG to the intended whole USB device with an imaging tool. This erases that device. Confirm its identity first.
4. Select the USB device from the firmware boot menu. See [Hardware](system-hardware.md).

## Update an existing checkout
Back up the persistent image, update the source, and run `cargo xtask run`. Never overwrite a personal disk with the clean release IMG to perform an update. There is no in-OS automatic updater in 0.7.

## Troubleshooting
For no bootable medium, check UEFI and image selection. If a disk layout is unsupported, preserve it and restore the backup; do not use `--fresh-disk` as a repair command. Actual validation platforms are listed in the release evidence, not implied by these setup instructions.
