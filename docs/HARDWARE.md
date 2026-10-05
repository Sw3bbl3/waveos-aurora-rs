# Running WaveOS Aurora on real hardware

For the versioned 0.7 user and developer guides, see the [bilingual handbook](handbook/README.md). This reference URL remains available.

WaveOS is developed and tested in QEMU. Milestone 5 adds the drivers a real PC needs: multicore, USB, sound, ACPI power management and sleep. None of it has yet been verified on physical machines. This page explains how to try it and what to check, and gives you a table to record the results.

## Make a boot stick

1. Build the image:
   ```sh
   cargo xtask image          # writes target/waveos-aurora-usb.img (256 MiB)
   ```
2. Write it to a USB stick. **This erases the stick.**
   - macOS:
     ```sh
     diskutil list                       # find the stick, e.g. /dev/disk4
     diskutil unmountDisk /dev/diskN
     sudo dd if=target/waveos-aurora-usb.img of=/dev/rdiskN bs=4m
     ```
   - Linux: `sudo dd if=target/waveos-aurora-usb.img of=/dev/sdX bs=4M conv=fsync`
3. In the PC's firmware setup:
   - boot in **UEFI** mode (not legacy/CSM)
   - disable **Secure Boot**, because the loader is unsigned
   - if the internal disk is in **RAID / Intel RST** mode, switch it to **AHCI** so WaveOS can see it (optional)
4. Boot from the stick. On many PCs a key at power-on opens the boot menu: F12, F11, F9 or Esc.

The stick is a complete system. Its **EFI partition** appears at `/Boot`, and its **"Aurora HD"** partition holds your files, so notes and settings persist on the stick. If the computer's own disk already has a WaveOS "Aurora HD" partition, that one is used instead.

## Boot options

Options go in `\aurora\boot.conf` on the EFI partition, one `key=value` per line. You can edit the file from another computer, since the partition is FAT32.

| Option | Effect |
|---|---|
| `resolution=1920x1080` | The display mode the bootloader picks (Settings → Display writes it) |
| `acpi=off` | Don't start the AML interpreter: no power button, battery or sleep, but everything else works. Try this if the machine misbehaves during boot. |
| `sleep=off` | Hide Sleep, for machines whose firmware does not resume properly |

## What to check

Work through the list and note what happens. The boot log (below) records the details.

- [ ] **Boots to the desktop.** Note the time it takes and whether the picture looks right.
- [ ] **Display.** Check the resolution, and whether the text is sharp.
- [ ] **All CPU cores.** In Terminal, `cpuinfo` should list every core; Activity Monitor shows a graph per core.
- [ ] **Keyboard.** Check the built-in keyboard and a USB keyboard, including a non-US layout (Settings → Keyboard) and key repeat.
- [ ] **Pointer.** Check the touchpad, a USB mouse (with the scroll wheel), and clicking and dragging windows.
- [ ] **USB stick.** Plug in a FAT32 stick: it should appear in Files under Locations. Open a file, copy one onto it, then eject it and unplug.
- [ ] **Internal disk.** `lspci` should list the controller, and `df` should show whether a WaveOS volume was found.
- [ ] **Network.** With a supported wired adapter (see below) plugged in, the menu-bar network item should show Connected and an address. In Terminal try `ifconfig`, `ping example.com` and `fetch https://example.com`, then open Nebula.
- [ ] **Sound.** Play `play /System/Sounds/startup.wav`, then `play --tone 440 1000`. Try the volume keys, the menu-bar slider, and headphones in and out.
- [ ] **Battery (laptops).** Check the menu-bar percentage and `battery` in Terminal. Unplug and replug the charger.
- [ ] **Power button.** A short press should open the Shut Down dialog.
- [ ] **Sleep.** Use Aurora menu → Sleep, then wake with the power button or a key. Also try closing the lid. Afterwards, check that the picture, the keyboard and pointer, the disk and sound all work.
- [ ] **Shut Down and Restart.** Both from the Aurora menu.

## Reporting what happened

- **The boot log.** A few seconds after the desktop appears, the kernel writes `\aurora\boot.log` to the EFI partition. It contains the CPU, the PCI and USB devices, and the full kernel log. Read it on another computer, or in Terminal:
  - `cat /Boot/aurora/boot.log`
  - `dmesg` for the live log
  - `lspci`, `lsusb`, `cpuinfo` and `battery` for the individual reports
- **A crash screen** shows the fault and where it happened. Please photograph it.
- **A hang before the desktop.** If the machine hangs early, try `acpi=off`. If it then boots, the problem is in AML: include the `aml:` lines from the log.

## Known limits

- **Graphics.** There is no native GPU driver: WaveOS draws into the framebuffer the firmware set up (GOP). The resolution changes only at boot. After sleep the screen may stay dark on some machines, because the firmware doesn't re-initialize the display; the rest of the system resumes.
- **Storage.** AHCI, NVMe and virtio disks work, but Intel RST / RAID mode does not. USB storage must be FAT32 (exFAT and NTFS aren't supported).
- **Input.**
  - Touchpads that connect over I²C (common on laptops since about 2016) aren't supported yet. Many of them also have a PS/2 fallback, which works.
  - USB keyboard LEDs (Caps Lock) don't light.
- **USB.**
  - xHCI (USB 3) controllers only. Older EHCI/OHCI-only machines rely on the firmware's legacy PS/2 emulation for the keyboard and mouse.
  - No isochronous devices, such as USB audio or webcams.
- **Sound.** Intel HD Audio output only, with no microphone and no HDMI audio.
- **Networking.** Wired Ethernet only, on Intel e1000/e1000e adapters (82540EM, 82545/82546, 82574L and similar) or virtio-net in virtual machines. Realtek and newer Intel (I219, I225) chips, and Wi-Fi, aren't supported yet. IPv4 only, with the address from DHCP.
- **Sleep** is S3 (suspend to RAM). Many machines since about 2019 support only "Modern Standby" (S0ix), which WaveOS doesn't implement. On those, Sleep isn't offered.

## Compatibility

Please add a row for each machine you try.

| Machine | CPU | Boots | Display | Keyboard / pointer | USB storage | Disk | Network | Sound | Battery | Sleep | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| QEMU 11 (q35, Homebrew OVMF), macOS host | TCG, 4 cores | ✅ | ✅ | ✅ PS/2, USB | ✅ | ✅ AHCI, NVMe, virtio | ✅ virtio-net, e1000, e1000e | ✅ HDA | ✅ test SSDT | ✅ | Development setup (`cargo xtask run` / `test`) |
| QEMU and OVMF from Ubuntu 24.04 | TCG, 4 cores | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | CI. This firmware build faults while resuming from S3, before WaveOS runs, so the tests skip sleep there |
| | | | | | | | | | | | |
