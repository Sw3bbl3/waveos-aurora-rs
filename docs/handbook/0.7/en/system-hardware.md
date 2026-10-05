# Hardware and power
The supported architecture is x86-64 with UEFI. Drivers include AHCI, modern virtio-blk, NVMe, xHCI USB keyboards/mice/tablets/storage, Intel HD Audio, and selected network adapters. Hardware support is device-specific, not a guarantee for every PC.

## Inspect hardware
1. Run `cpuinfo`, `lspci`, and `lsusb` in Terminal.
2. Run `battery` on a laptop or use QEMU’s `--battery` test configuration.
3. Save `dmesg` output when diagnosing a device.
4. Consult the release validation for configurations actually exercised.

## Sleep and shutdown
Save work before selecting Sleep. S3 depends on firmware and driver resume support. Modern Standby, I²C touchpads, and a native GPU driver remain unavailable. A display may not resume on unsupported hardware.

## Troubleshooting
Use a USB keyboard/mouse when a built-in touchpad is unsupported. If sleep fails, restart from a stopped VM or firmware boot menu and inspect logs. Do not interpret QEMU results as physical-hardware certification.
