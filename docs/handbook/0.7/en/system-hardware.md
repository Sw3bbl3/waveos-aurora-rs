# Hardware and power
The supported architecture is x86-64 with UEFI. Drivers include AHCI, modern virtio-blk, NVMe, xHCI USB keyboards/mice/tablets/storage, Intel HD Audio, and selected network adapters. Hardware support is device-specific, not a guarantee for every PC.

## What a PC needs
- A 64-bit Intel or AMD processor and UEFI firmware with Secure Boot off; legacy BIOS can't start WaveOS.
- 512 MiB of memory for the desktop and WaveOS's apps, **2 GiB for Voyage or Firefox**, 4 to 8 GiB recommended. There is no swap: a program that runs out of memory fails.
- A USB stick or disk of 2 GiB with the Linux packages (512 MiB without them); 8 GiB or more is recommended.
- For the network, an Intel e1000 or e1000e wired adapter.

The full table is in `docs/HARDWARE.md` in the WaveOS source.

## Inspect hardware
1. Run `cpuinfo`, `lspci`, and `lsusb` in Terminal.
2. Run `battery` on a laptop or use QEMU’s `--battery` test configuration.
3. Save `dmesg` output when diagnosing a device.
4. Consult the release validation for configurations actually exercised.

## Sleep and shutdown
Save work before selecting Sleep. S3 depends on firmware and driver resume support. Modern Standby, I²C touchpads, and a native GPU driver remain unavailable. A display may not resume on unsupported hardware.

## Troubleshooting
Use a USB keyboard/mouse when a built-in touchpad is unsupported. If sleep fails, restart from a stopped VM or firmware boot menu and inspect logs. Do not interpret QEMU results as physical-hardware certification.
