# Running WaveOS Aurora on real hardware

For the versioned 0.7 user and developer guides, see the [bilingual handbook](handbook/README.md). This reference URL remains available.

WaveOS is developed and tested in QEMU. Milestone 5 adds the drivers a real PC needs: multicore, USB, sound, ACPI power management and sleep. None of it has yet been verified on physical machines. This page explains how to try it and what to check, and gives you a table to record the results.

## What you need

| | Desktop, WaveOS apps and NetSurf | Voyage or Firefox too | Recommended |
|---|---|---|---|
| Processor | Any 64-bit Intel or AMD (x86-64), 1 core | 2 or more cores | 4 or more cores with AVX2 (Intel from 2013, AMD Ryzen) |
| Memory | 512 MiB (it boots on 256 MiB) | **2 GiB** | 4 to 8 GiB |
| USB stick or disk | 512 MiB (`cargo xtask image --no-linux-root`) | 2 GiB; 4 GiB leaves room for your files | 8 GiB or more, USB 3 or an SSD in AHCI mode |
| Network | Optional | Intel e1000/e1000e (see Known limits) | Intel 82574L, for example an Intel Gigabit CT PCIe card |
| Screen | 1280 × 800 | 1920 × 1080 | 1920 × 1080; larger screens work, but the processor draws everything |

- **Firmware:** 64-bit UEFI with Secure Boot off. Legacy BIOS (CSM) can't start WaveOS.
- **Processor:** WaveOS uses up to 16 cores and parks the rest. AVX is used when the processor has it (WebKit's JavaScript compiler benefits) but isn't required.
- **Memory:** WaveOS has no swap, so a program that runs out of memory fails. The desktop uses about 230 MiB; with Voyage showing a Wikipedia article, about 850 MiB in all. With 1 GiB, Voyage can't load WebKit and doesn't open. These figures come from QEMU at 1920 × 1080.
- **Apple computers:** Macs with Apple silicon can't start WaveOS (it's built for x86-64); run it in a virtual machine there.

The machines most likely to work are desktop PCs from about 2012 to 2020 with the disk in AHCI mode, a USB keyboard and mouse, and an Intel 82574L network card. Laptops are a harder fit: their touchpads, Wi-Fi and sleep are often the kinds WaveOS doesn't drive yet.

## Make a boot stick

1. Build the image:
   ```sh
   cargo xtask image          # writes target/waveos-aurora-usb.img (about 1.7 GiB)
   ```
   The image holds the Linux packages Voyage and Firefox need. `cargo xtask image --no-linux-root` leaves them out and makes a 256 MiB image, with NetSurf as the browser.
2. Write it to a USB stick of at least 2 GiB (512 MiB without the Linux packages). **This erases the stick.**
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
- [ ] **Pointer.** Check the touchpad, a USB mouse (with the scroll wheel and, if it has them, the back and forward buttons), and clicking and dragging windows. On a USB touchpad, check tap to click, two-finger scrolling and the two-finger click.
- [ ] **Media keys and Caps Lock.** Volume and play/pause keys should work; the Caps Lock light should follow the key on every keyboard.
- [ ] **Game controllers.** Plug in an Xbox, PlayStation or generic USB controller; `lsusb` should list it as a game controller.
- [ ] **USB stick.** Plug in a FAT32 stick: it should appear in Files under Locations. Open a file, copy one onto it, then eject it and unplug.
- [ ] **Internal disk.** `lspci` should list the controller, and `df` should show whether a WaveOS volume was found.
- [ ] **Network.** With a supported wired adapter (see below) plugged in, the menu-bar network item should show Connected and an address. In Terminal try `ifconfig`, `ping example.com` and `fetch https://example.com`, then open NetSurf.
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

- **Graphics.** On real hardware there is no native GPU driver yet: WaveOS draws into the framebuffer the firmware set up (GOP). In virtual machines with a virtio display, Linux's virtio-gpu driver (run through WaveOS's compatibility layer) drives it. The resolution changes only at boot. After sleep the screen may stay dark on some machines, because the firmware doesn't re-initialize the display; the rest of the system resumes.
- **Storage.** AHCI, NVMe and virtio disks work, but Intel RST / RAID mode does not. USB storage must be FAT32 (exFAT and NTFS aren't supported).
- **Input.**
  - Touchpads that connect over I²C (common on laptops since about 2016) aren't supported yet. Many of them also have a PS/2 fallback, which works (as a plain pointer, without gestures). USB Precision Touchpads get the full gestures.
  - Bluetooth keyboards, mice and controllers aren't supported.
- **USB.**
  - xHCI (USB 3), EHCI (USB 2) with its OHCI or UHCI companions, and OHCI and UHCI on their own (USB 1.1) are all driven. EHCI, OHCI and UHCI have only been tested in QEMU.
  - No isochronous devices, such as USB audio or webcams.
- **Sound.** Intel HD Audio output only, with no microphone and no HDMI audio.
- **Networking.** Wired Ethernet only, on Intel e1000/e1000e adapters (82540EM, 82545/82546, 82574L and similar) or virtio-net in virtual machines. Realtek and newer Intel (I219, I225) chips aren't supported yet. Wi-Fi: Intel adapters from the Wireless 7265 to the Wi-Fi 6E AX211 are recognized, but WaveOS can't start their radio yet, so no real Wi-Fi works. IPv4 only, with the address from DHCP.
- **Sleep** is S3 (suspend to RAM). Many machines since about 2019 support only "Modern Standby" (S0ix), which WaveOS doesn't implement. On those, Sleep isn't offered.

## Compatibility

Please add a row for each machine you try.

| Machine | CPU | Boots | Display | Keyboard / pointer | USB storage | Disk | Network | Sound | Battery | Sleep | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| QEMU 11 (q35, Homebrew OVMF), macOS host | TCG, 4 cores | ✅ | ✅ | ✅ PS/2, USB | ✅ | ✅ AHCI, NVMe, virtio | ✅ virtio-net, e1000, e1000e | ✅ HDA | ✅ test SSDT | ✅ | Development setup (`cargo xtask run` / `test`) |
| QEMU 8.2 (q35, Ubuntu OVMF), Linux host | TCG, 4 cores | ✅ | ✅ | ✅ PS/2, USB on xHCI, EHCI+UHCI, EHCI+OHCI, OHCI, UHCI; virtual touchpad, keyboard and controllers over usb-redir | ✅ every controller | ✅ AHCI | ✅ virtio-net | ✅ HDA | ✅ test SSDT | ⚠️ firmware doesn't resume | `cargo xtask test --usb …` |
| QEMU and OVMF from Ubuntu 24.04 | TCG, 4 cores | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | CI. This firmware build faults while resuming from S3, before WaveOS runs, so the tests skip sleep there |
| | | | | | | | | | | | |
