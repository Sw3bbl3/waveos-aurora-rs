# Running WaveOS Aurora on real hardware

For the versioned 0.7 user and developer guides, see the [bilingual handbook](handbook/README.md). This reference URL remains available.

WaveOS is developed and tested in QEMU. Milestone 5 adds the drivers a real PC needs: multicore, USB, sound, ACPI power management and sleep. None of it has yet been verified on physical machines. This page explains how to try it and what to check, and gives you a table to record the results.

## What you need

| | Desktop, WaveOS apps and NetSurf | Voyage or Firefox too | Recommended |
|---|---|---|---|
| Processor | Any 64-bit Intel or AMD (x86-64), 1 core | 2 or more cores | 4 or more cores with AVX2 (Intel from 2013, AMD Ryzen) |
| Memory | 512 MiB (it boots on 256 MiB) | **2 GiB** | 4 to 8 GiB |
| USB stick or disk | 512 MiB (`cargo xtask image --no-linux-root`) | 4 GiB | 8 GiB or more, USB 3 or an SSD in AHCI mode |
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
   cargo xtask image          # writes target/waveos-aurora-usb.img (about 1.8 GiB)
   ```
   The image holds the Linux packages Voyage and Firefox need. `cargo xtask image --no-linux-root` leaves them out and makes a 256 MiB image, with NetSurf as the browser.
2. Write it to a USB stick of at least 4 GB (512 MiB without the Linux packages). **This erases the stick.**
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
| `i2c=off` | Leave the I²C controllers alone (no I²C touchpad or touch screen). The `i2c:` lines in the log still list what the firmware describes. |

## What to check

Work through the list and note what happens. The boot log (below) records the details.

- [ ] **Boots to the desktop.** Note the time it takes and whether the picture looks right.
- [ ] **Display.** Check the resolution, and whether the text is sharp.
- [ ] **All CPU cores.** In Terminal, `cpuinfo` should list every core; Activity Monitor shows a graph per core.
- [ ] **Keyboard.** Check the built-in keyboard and a USB keyboard, including a non-US layout (Settings → Keyboard) and key repeat.
- [ ] **Pointer.** Check the touchpad, a USB mouse (with the scroll wheel and, if it has them, the back and forward buttons), and clicking and dragging windows. On a USB touchpad, check tap to click, two-finger scrolling and the two-finger click.
- [ ] **Built-in touchpad or touch screen over I²C (laptops).** See [I²C touchpads](#i2c-touchpads-and-touch-screens) below: whether the pointer moves, tap to click, two-finger scrolling, clicking the pad, and a three-finger swipe sideways (back and forward in the browser).
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

## I²C touchpads and touch screens

Most laptops since about 2016 connect the touchpad (and any touch screen) to an I²C controller in the chipset; only ACPI says where. WaveOS reads that from the firmware's tables, drives the controller (the Synopsys DesignWare I²C block in Intel LPSS and AMD chipsets) and speaks Microsoft's HID over I²C protocol to the device. A Precision Touchpad then behaves exactly like a USB one: the same gestures and the same Settings → Trackpad options (tap to click, natural scrolling, tracking speed). Touch screens move the pointer to the finger and click while it touches.

**This is untested on real hardware.** The protocol, the resource decoding and the controller driver were written from the public specifications and checked against simulations (host tests) and a software touchpad on a simulated bus (the kernel self-test, through the real ACPI tables and driver). If the touchpad doesn't work, the PS/2 fallback many laptops have still does (as a plain pointer), and `i2c=off` turns the I²C drivers off.

How the driver knows a report is waiting:

| The firmware describes | WaveOS does |
|---|---|
| `Interrupt` (an I/O APIC input) | real interrupts |
| `GpioInt` on an AMD GPIO controller (`AMDI0030`) | reads the pin's level (no bus traffic) and the bus only when it is asserted |
| `GpioInt` on an Intel GPIO controller (most Intel laptops) | **polls**: 125 reads a second while a finger is on the pad, 20 a second when idle. The first touch can take up to 50 ms to notice, and the touchpad and controller never sleep, which costs a little battery. |

Please send back the lines starting with `i2c:` from the boot log (and the `== I2C ==` section near its top). They name each touchpad's ACPI path and ID, its controller, address and speed, the HID descriptor register, the interrupt resource and which of the three modes above was used, the controller's component version, FIFO depths and bus timings, and the touchpad's USB-style vendor and product id, report descriptor size and whether it accepted touchpad mode. Lines such as `not responding`, `the device stopped answering`, `not a DesignWare I2C controller` or `no driver for this controller` say where it stopped.

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
  - Touchpads and touch screens that connect over I²C (common on laptops since about 2016) have a driver that has not yet run on real hardware (see [above](#i2c-touchpads-and-touch-screens)); on most Intel laptops it polls the touchpad, as there is no GPIO driver yet. Many such touchpads also have a PS/2 fallback, which works as a plain pointer. I²C keyboards and pens aren't supported. USB Precision Touchpads get the full gestures.
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
