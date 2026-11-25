# Running WaveOS Aurora on real hardware

For the versioned 0.7 user and developer guides, see the [bilingual handbook](handbook/README.md). This reference URL remains available.

WaveOS is developed and tested in QEMU. Milestone 5 adds the drivers a real PC needs: multicore, USB, sound, ACPI power management and sleep. None of it has yet been verified on physical machines. This page explains how to try it and what to check, and gives you a table to record the results.

## What you need

| | Desktop, WaveOS apps and NetSurf | Voyage or Firefox too | Recommended |
|---|---|---|---|
| Processor | Any 64-bit Intel or AMD (x86-64), 1 core | 2 or more cores | 4 or more cores with AVX2 (Intel from 2013, AMD Ryzen) |
| Memory | 512 MiB (it boots on 256 MiB) | **2 GiB** | 4 to 8 GiB |
| USB stick or disk | 512 MiB (`cargo xtask image --no-linux-root`) | 4 GiB | 8 GiB or more, USB 3 or an SSD in AHCI mode |
| Network | Optional | Intel e1000/e1000e or igb, Realtek RTL8139 or RTL8168/8111 (see Known limits) | Intel 82574L, for example an Intel Gigabit CT PCIe card |
| Screen | 1280 × 800 | 1920 × 1080 | 1920 × 1080; larger screens work, but the processor draws everything |

- **Firmware:** 64-bit UEFI with Secure Boot off. Legacy BIOS (CSM) can't start WaveOS.
- **Processor:** WaveOS uses up to 16 cores and parks the rest. AVX is used when the processor has it (WebKit's JavaScript compiler benefits) but isn't required.
- **Memory:** When memory runs short WaveOS compresses pages programs haven't used lately, then moves them to an encrypted swap file, and as a last resort ends a background program ([how](developer/memory.md)). The desktop uses about 230 MiB; with Voyage showing a Wikipedia article, about 850 MiB in all. These figures come from QEMU at 1920 × 1080, measured before swap existed (when Voyage could not open in 1 GiB); they have not been measured again since.
- **Apple computers:** Macs with Apple silicon can't start WaveOS directly (their firmware isn't UEFI); run it in a virtual machine there, where the 64-bit Arm build runs at the processor's own speed (`make run`; see [64-bit Arm](#64-bit-arm)).

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
| `gpu=linux` | Drive a virtio GPU with the ported Linux driver instead of the native one (virtual machines) |
| `cursor=software` | Draw the pointer into the picture instead of on the display's cursor plane (on Intel graphics this also leaves the display buffer as the firmware set it up) |
| `intel=off` | Leave Intel graphics to the firmware framebuffer, as before the Intel driver existed |
| `intel=modeset` | Let the Intel driver change resolutions (Skylake, Kaby, Coffee and Comet Lake, external displays only, no larger than the firmware's mode; untested) |
| `amd=off` | Leave AMD Radeon graphics to the firmware framebuffer, as before the AMD driver existed. (`amd=modeset` is accepted but changes nothing: the AMD driver can't set modes.) |
| `nvidia=off` | Don't read the Nvidia GPU's identification register (the log then names the GPU from its device id only). Nvidia GPUs always use the firmware framebuffer. |
| `timer=periodic` | Go back to a 1 kHz tick on every CPU (instead of tickless timers). `timer=oneshot` avoids the TSC-deadline timer. Try these if the machine hangs or stutters once the desktop is up. |
| `idle=halt` | Idle with `hlt` only (no `mwait` C-states). `idle=mwait` uses `mwait` even in a virtual machine; `idle=poll` never sleeps (for measurements). |
| `sdhci=pio` | How SD cards and eMMC move data: `pio`, `sdma`, `adma32` or `adma64` (default: the best the controller offers). Try `sdhci=pio` if a card reader corrupts data or hangs. |
| `sdhci_speed=high` | Caps the SD/eMMC bus: `high` (no UHS-I, HS200 or HS400), `default` (25 MHz), `slow` (12.5 MHz). |
| `emmc=hs400` | Allows eMMC HS400 (off by default: whether a controller can do it isn't discoverable). |
| `usbnet=ecm` | Which configuration a USB network device with several runs in: `ecm`, `ncm`, `eem`, `rndis` or `vendor` (default: NCM, then ECM, then a vendor chip with a driver, EEM, RNDIS). See [USB network adapters](#usb-network-adapters). |

## What to check

Work through the list and note what happens. The boot log (below) records the details.

- [ ] **Boots to the desktop.** Note the time it takes and whether the picture looks right.
- [ ] **Display.** Check the resolution, and whether the text is sharp. On Intel, AMD or Nvidia graphics, see [Intel graphics](#intel-graphics), [AMD Radeon graphics](#amd-radeon-graphics) or [Nvidia GeForce graphics](#nvidia-geforce-graphics) below.
- [ ] **All CPU cores.** In Terminal, `cpuinfo` should list every core; Activity Monitor shows a graph per core.
- [ ] **Keyboard.** Check the built-in keyboard and a USB keyboard, including a non-US layout (Settings → Keyboard) and key repeat.
- [ ] **Pointer.** Check the touchpad, a USB mouse (with the scroll wheel and, if it has them, the back and forward buttons), and clicking and dragging windows. On a USB touchpad, check tap to click, two-finger scrolling and the two-finger click.
- [ ] **Built-in touchpad or touch screen over I²C (laptops).** See [I²C touchpads](#i2c-touchpads-and-touch-screens) below: whether the pointer moves, tap to click, two-finger scrolling, clicking the pad, and a three-finger swipe sideways (back and forward in the browser).
- [ ] **Media keys and Caps Lock.** Volume and play/pause keys should work; the Caps Lock light should follow the key on every keyboard.
- [ ] **Game controllers.** Plug in an Xbox, PlayStation or generic USB controller; `lsusb` should list it as a game controller.
- [ ] **USB stick.** Plug in a FAT32 stick: it should appear in Files under Locations. Open a file, copy one onto it, then eject it and unplug.
- [ ] **Internal disk.** `lspci` should list the controller, and `df` should show whether a WaveOS volume was found.
- [ ] **SD card reader and eMMC.** See [SD cards and eMMC](#sd-cards-and-emmc): a FAT32 card should appear in Files when inserted and go away when pulled out; `dmesg | grep sdhci` describes the controller and card.
- [ ] **Network.** With a supported wired adapter (see below) plugged in, the menu-bar network item should show Connected and an address. In Terminal try `ifconfig`, `ping example.com` and `fetch https://example.com`, then open NetSurf.
- [ ] **USB network adapter, dock or tethering.** Plug in a USB Ethernet adapter or dock with a cable, or a phone with USB tethering turned on (see [USB network adapters](#usb-network-adapters)). Within a few seconds `ifconfig` should show it (`eth…` or `usb…`) with an address, and Settings → Network its driver and speed. Start a download, pull it out: the download should fail at once and another interface (if any) take over; plug it back in. Please report the `usbnet:` and `usb:` lines from `dmesg` and the adapter's line in `lsusb`.
- [ ] **Sound.** Play `play /System/Sounds/startup.wav`, then `play --tone 440 1000`. Try the volume keys, the menu-bar slider, and headphones in and out.
- [ ] **USB audio.** Plug in a USB headset, speaker or DAC: `lsusb` should show it as `audio: speaker` (and `microphone`), and `record --list` should list it. Choose it in the menu bar's sound card or Settings → Sound, play a sound, change the volume, then `record -t 5 test.wav` and `play test.wav`. Unplug it while it plays: sound should move back to the built-in output.
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

## Intel graphics

Laptops and desktops with Intel integrated graphics from Skylake (6th generation Core, 2015) to Raptor Lake (13th/14th generation, 2023), Alder Lake-N and the Atom-class Apollo, Gemini, Elkhart and Jasper Lake get a native display driver. It keeps the mode the firmware set at boot (no resolution change, no flicker), shows WaveOS's picture straight from memory, adds a hardware pointer, reads the monitors' EDID, notices monitors plugged in and out, and lets the brightness keys set the internal panel's backlight.

**This is untested on real hardware.** It was written from Intel's public Programmer's Reference Manuals and checked against a simulated display engine (host tests and a kernel self-test). It reads everything first and declines (leaving the firmware framebuffer in charge, exactly as before) if anything about the firmware's setup is unexpected. The details are in [developer/intel-display.md](developer/intel-display.md).

| Platform | Display version | Device ids | Default (takeover) | `intel=modeset` |
|---|---|---|---|---|
| Skylake, Kaby Lake, Kaby Lake R, Amber Lake, Coffee Lake, Whiskey Lake, Comet Lake | 9 | 8086:19xx, 59xx, 87c0, 3e9x, 3eax, 87ca, 9bxx | Untested on real hardware | Untested on real hardware |
| Apollo Lake, Gemini Lake | 9, 10 | 8086:0a84, 1a84, 1a85, 5a84, 5a85, 3184, 3185 | Untested on real hardware | — |
| Ice Lake, Elkhart Lake, Jasper Lake | 11 | 8086:8a5x, 8a70, 8a71, 45xx, 4exx | Untested on real hardware | — |
| Tiger Lake, Rocket Lake, Alder Lake-S, Raptor Lake-S | 12 | 8086:9a4x–9afx, 4c8x, 4c9x, 468x, 469x, a78x | Untested on real hardware | — |
| Alder Lake-P, Alder Lake-N, Raptor Lake-P/U | 13 | 8086:46ax, 46bx, 46cx, 462x, 46dx, a72x, a7ax | Untested on real hardware | — |

Not driven (the firmware framebuffer as before): Broadwell and older, Meteor Lake and newer, Arc and other discrete GPUs. Monitors the firmware didn't light at boot are detected and listed, but can't be turned on yet; the resolution still changes at the next start (Settings → Display), as before. After sleep the screen may stay dark, as with the firmware framebuffer.

What to try and send back:

1. Boot without any `intel=` option. The desktop should look exactly as it did before, at the same resolution.
2. In the boot log (`\aurora\boot.log`, or `dmesg`), every line starting `intel:`. They name the GPU and platform, the graphics memory layout, each display the firmware lit (pipe, connection type, port, resolution, refresh, link lanes), the pointer's display buffer share or why there is none, the backlight, each connector with its monitor's name, and either `took the display over from the firmware` or `declining: …` with the reason.
3. Check:
   - [ ] no flicker, flashing lines or corruption, especially around the pointer and while dragging windows
   - [ ] the pointer's shape, and that clicks land at its tip
   - [ ] the brightness keys (laptops)
   - [ ] plugging a monitor in and out: an `intel: HDMI-A-1: Connected` (or `DP-1`) line should appear within a few seconds
4. If something is wrong, try `cursor=software`, then `intel=off`, and say which one fixed it.
5. Optional, on a Skylake to Comet Lake machine with an external monitor: add `intel=modeset`, choose a smaller resolution in Settings → Display, and send the `intel:` lines about it.
6. Optional, with the Linux packages on the disk: run `kmscube` in Terminal (Return ends it). Mesa draws in software (llvmpipe) on any display device, but full-screen programs on the Intel driver, and llvmpipe's code generation on real processors (it picks instructions by `cpuid`, which `/proc/cpuinfo` now reports in full), have only run in QEMU. The `drm: kmscube on card0: …` line in the log gives its frame rate.

## AMD Radeon graphics

Ryzen laptops and desktops with integrated Radeon graphics (Raven Ridge 2018 through Phoenix and Hawk Point 2024, Raphael and Granite Ridge desktop iGPUs, the Steam Deck's Van Gogh) and Radeon cards from Polaris (RX 400/500) through Vega and RDNA 1–3 (RX 5000, 6000, 7000) get a native display driver. Like the Intel driver it keeps the mode the firmware set at boot; it shows WaveOS's picture from two buffers of its own in video memory, flipping between them at the vertical blank, adds a hardware pointer, reads DisplayPort monitors' EDID and notices monitors plugged in and out. It cannot change the resolution (that needs the card's own AtomBIOS tables, which WaveOS doesn't run): Settings → Display still applies a new resolution at the next start, as before.

**This is untested on real hardware.** It was written from AMD's published register headers and checked against a simulated display engine (host tests and a kernel self-test). Before writing anything it reads which displays are lit and what they scan, and declines (leaving the firmware framebuffer in charge, exactly as before) unless that is exactly the framebuffer the bootloader was given. The details are in [developer/amd-display.md](developer/amd-display.md).

| GPUs | Display block | Device ids | Default (takeover) |
|---|---|---|---|
| Polaris 10/11/12/20/30 (RX 460–590, RX 540/550, WX 2100–7100) | DCE 11.2 | 1002:67cx, 67dx, 67ex, 67ff, 698x, 6995, 699f, 6fdf | Untested on real hardware |
| Vega 10/12/20 (RX Vega 56/64, Radeon VII) | DCE 12 | 1002:686x, 687f, 69ax, 66ax | Untested on real hardware |
| Raven Ridge, Picasso (Ryzen 2000/3000 APUs) | DCN 1.0 | 1002:15dd, 15d8 | Untested on real hardware |
| Renoir, Lucienne, Cezanne, Barcelo (Ryzen 4000/5000 APUs) | DCN 2.1 | 1002:1636, 164c, 1638, 15e7 | Untested on real hardware |
| Van Gogh (Steam Deck) | DCN 3.0.1 | 1002:163f | Untested on real hardware |
| Rembrandt (Ryzen 6000) | DCN 3.1.2 | 1002:164d, 1681 | Untested on real hardware |
| Phoenix, Hawk Point (Ryzen 7040/8040) | DCN 3.1.4 | 1002:15bf, 15c8, 164f, 1900, 1901 | Untested on real hardware |
| Raphael, Granite Ridge (Ryzen 7000/9000 desktop graphics) | DCN 3.1.5 | 1002:164e, 13c0 | Untested on real hardware |
| Mendocino (Ryzen 7020) | DCN 3.1.6 | 1002:1506 | Untested on real hardware |
| Navi 10/12/14 (RX 5500–5700 XT) | DCN 2.0 | 1002:731x, 734x, 736x | Untested on real hardware |
| Navi 21/22/23/24 (RX 6400–6950 XT) | DCN 3.0–3.0.3 | 1002:73a0–73ff, 7420–743f | Untested on real hardware |
| Navi 31/32/33 (RX 7600–7900 XTX) | DCN 3.2, 3.2.1 | 1002:7440–749f, 73f0 | Untested on real hardware |

Not driven (the firmware framebuffer as before, with a log line saying why): older Radeons (HD 7000 to R9 Fury, DCE 6–10), Carrizo and Stoney, Strix Point and newer APUs, RX 9000 (RDNA 4). Monitors the firmware didn't light at boot are detected and listed (with their EDID when they are DisplayPort), but can't be turned on. HDMI monitors' EDID isn't read. After sleep the screen may stay dark, as with the firmware framebuffer.

What to try and send back:

1. Boot without any `amd=` option. The desktop should look exactly as it did before, at the same resolution.
2. In the boot log (`\aurora\boot.log`, or `dmesg`), every line starting `amd:`. They name the GPU, its display block and whether the register layout is confirmed for it, the VRAM aperture and registers, where the firmware framebuffer is and which part of video memory the driver uses, each lit display (timing generator, pipes, resolution, refresh, pitch), whether the hardware pointer is used, each connector, and either `took the display over from the firmware` or `declining: …` with the reason. A machine with two GPUs logs both; only the one showing the boot screen is taken over.
3. Check:
   - [ ] no flicker, corruption or garbage on screen, especially right after the desktop appears (that would mean the video memory the driver chose was not free) and while dragging windows
   - [ ] the pointer: visible, the right shape and colours (wrong colours or a dark fringe: the cursor mode is off), clicks landing at its tip, and moving off the left and top edges
   - [ ] plugging a monitor in and out: an `amd: DP-2: Connected` (or `Unknown-2`) line should appear within a few seconds
4. If something is wrong, try `cursor=software` (no hardware pointer), then `amd=off`, and say which one fixed it.

## Nvidia GeForce graphics

GeForce GPUs from the GTX 900 series (Maxwell) to the RTX 40 series (Ada) are **recognised but not driven**: WaveOS keeps using the firmware framebuffer, at the resolution the bootloader chose, with the pointer drawn in software. A native driver needs Nvidia's GSP firmware and much that isn't publicly documented; [developer/nvidia-display.md](developer/nvidia-display.md) explains what one would take. Please send the `nvidia:` line from the boot log: it names the GPU's architecture and chip (from its identification register; `nvidia=off` skips that read).

## SD cards and eMMC

WaveOS drives SD Host Controllers (SDHCI, the standard most laptop card readers and every x86 eMMC controller follow), found as PCI functions of class 08/05 or as ACPI devices (`PNP0D40`, Intel Bay Trail/Cherry Trail/LPSS and Apollo Lake ids, AMD `AMDI0040`/`AMDI0041`). SD cards (SDSC, SDHC, SDXC) and eMMC work as disks: the system can be installed to and run from either, and a removable card's FAT32 volumes appear in Files under Locations when it is inserted and disappear when it is pulled out (or ejected). eMMC boot partitions and the RPMB partition are never read or written, and never shown.

**What has run, and where.** The driver core (`libs/sdmmc`) runs on the host against a simulated controller and cards (`cargo test -p aurora-sdmmc`) and in the kernel self-test against the same simulation; QEMU provides the real controller for the rest. Nothing has run on physical hardware yet.

| Feature | QEMU (`sdhci-pci`) | Simulation only | Not run at all |
|---|---|---|---|
| PCI discovery, register BARs, INTx interrupts (polled until ACPI routes them) | ✅ | | MSI (QEMU's has none) |
| ACPI-enumerated controllers (`_CRS` registers and interrupt) | | | ✅ untested |
| Controller versions 2.00 (8-bit divider) and 3.00 (10-bit divider) | ✅ | | 4.x controllers in their 4.0 mode (the driver uses the 3.00-compatible register layout throughout) |
| PIO, SDMA, ADMA2 32-bit and 64-bit descriptors | ✅ (64-bit on the version 3 reader) | SDMA stopping at its 512 KiB boundary | |
| SD init (CMD0/8/ACMD41/2/3/9/7), SDSC byte and SDHC block addressing, 4-bit bus, high speed (CMD6) | ✅ (128 MiB SDSC, 4 GiB SDHC) | | SDXC above 32 GB (same code path as SDHC) |
| Multi-block with Auto CMD12 | ✅ (QEMU's SCR has no CMD23) | | |
| Multi-block bounded by CMD23 | ✅ (eMMC) | ✅ (SD) | |
| UHS-I: 1.8 V switch (CMD11), SDR50/SDR104/DDR50, tuning (CMD19) | | ✅ | ✅ real 1.8 V regulators, preset values |
| eMMC init (CMD1, EXT_CSD), 8-bit bus, high speed, byte-addressed eMMC up to 2 GB | ✅ | | |
| eMMC above 2 GB (sector mode), HS200 and tuning (CMD21), HS400 | | ✅ | ✅ on real parts |
| eMMC boot and RPMB partitions left alone and hidden | ✅ (the harness checks QEMU's backing image) | ✅ | |
| eMMC partition access left on a boot partition by firmware, switched back to the user area | | ✅ | |
| eMMC cache on and flushed; TRIM; erase in whole erase groups | erase on SD ✅ (QEMU's eMMC has no cache or TRIM) | ✅ | |
| Card insertion and removal | ✅ (QMP eject and change on the second reader) | ✅ | real card-detect switches and debounce |
| Write-protect switch | | ✅ | QEMU refuses read-only cards |
| Errors: command timeout and CRC, data CRC and timeout, ADMA error; retries, re-tuning, falling back to slower modes | | ✅ (injected) | |
| Resume from S3 (controller reset, card brought up again) | ✅ | | |

**Throughput in QEMU** (the self-test's `sd-perf:` lines; QEMU 11.1 under TCG on an 8-core Mac shared with other work, so only the ratios mean much). The card is QEMU's `sd-card` on the version 2.00 reader, 4-bit high speed at 26 MHz; sequential runs move 4 MiB in 256 KiB requests, random ones 200 single 4 KiB requests:

| Path | Sequential read | Sequential write | Random 4 KiB read | Random 4 KiB write |
|---|---|---|---|---|
| SD, PIO | 3.2–4.1 MB/s | 3.7–4.2 MB/s | 870–973 IOPS | 907–1015 IOPS |
| SD, SDMA | 6.6–7.7 MB/s | 6.6–7.6 MB/s | 1545–1870 IOPS | 1410–1765 IOPS |
| SD, ADMA2 (32-bit) | 6.3–8.1 MB/s | 6.1–7.6 MB/s | 1460–1850 IOPS | 1430–1770 IOPS |
| SD system disk (`--disk sd`: version 3.00 reader, ADMA2 64-bit) | 6.2–7.9 MB/s | 6.1–7.4 MB/s | 1450–1710 IOPS | 1350–1400 IOPS |
| AHCI system disk (for comparison) | 640–1060 MB/s | 550–740 MB/s | 14 000–33 000 IOPS | 15 000–28 000 IOPS |

DMA roughly doubles PIO here, where every 32-bit Buffer Data Port access is a trap into QEMU; SDMA and ADMA2 tie because QEMU's card model moves data a byte at a time either way, which also caps every SD figure two orders of magnitude below AHCI (whose reads come from the host's page cache). On real hardware the card's bus is the limit (about 23 MB/s at high speed, 90 MB/s at SDR104, 200 MB/s for HS200 eMMC), and ADMA2's single descriptor table per command should matter more than it does here.

In the boot log, `sdhci:` lines name each controller (version, base clock, DMA mode, 8-bit, 1.8 V and UHS-I support, removable or embedded, interrupt), each card (kind, name, size, bus width, timing and clock, CID) and any recovery (`I/O failed (…), attempt N`, `brought up again: …` after falling back to a slower mode). Please include them in reports. If a card reader misbehaves, try `sdhci=pio`, then `sdhci_speed=high` or `default`.

**Card readers that aren't SDHCI** are not supported. The boot log names them (`… card reader: not supported`):

| Reader | PCI ids | What a driver would need |
|---|---|---|
| Realtek RTS5209, RTS5227, RTS5228, RTS5229, RTS522A, RTS5249, RTS524A, RTS525A, RTS5260, RTS5261, RTS5264, RTL8402, RTL8411, RTL8411B (very common in laptops) | 10ec:5209, 5227, 5228, 5229, 522a, 5249, 524a, 525a, 5260, 5261, 5264, 5286, 5287, 5289 | Realtek's own PCIe interface: a command/response ring in host memory (`RTSX_HCBAR`/`HCBCTLR`) through which the driver reads and writes the chip's internal registers and sends SD commands (`SD_CMD0`–`SD_CMD5`, `SD_TRANSFER`), one scatter-gather table for data, per-chip initialisation (PHY registers over `PHYDATA`/`PHYADDR`, the card power and OCP controls, LDO and SSC clock settings that differ for each of the models above), card detect from `RTSX_BIPR`, and the 1.8 V switch through the chip's own LDO registers. The SD protocol in `libs/sdmmc/src/card.rs` would sit on top unchanged, behind a second host implementation. |
| Alcor Micro AU6601, AU6621, AU6625 | 1aea:6601, 6621, 6625 | A register interface of its own (command, block, DMA and interrupt registers at fixed offsets, a 4 KiB DMA page per transfer, PIO for short ones), with the same SD protocol on top. |
| USB card readers (Realtek RTS5129/5139/5179, Genesys GL3224 and others) | — | Nothing: they are USB mass storage and already work. |

O2 Micro, Genesys Logic GL9750/GL9755/GL9763E, JMicron JMB38x, Ricoh and BayHub readers present themselves as SDHCI and are driven, but without the per-vendor quirks other systems apply (for example GL975x clock and ASPM settings, JMicron's ADMA length limits). If one misbehaves, `sdhci=pio` is the first thing to try.

## Power management

Timers, idle states and processor frequency were written from the Intel SDM, AMD's manuals and the ACPI specification, and only the parts QEMU emulates have run: one-shot LAPIC timers, tickless idle with `hlt`, the ACPI paths finding no `_CST`/`_PSS`. **TSC-deadline timers, `mwait` C-states, Intel HWP, AMD CPPC and ACPI P-states have not run on real hardware.**

In the boot log, look for:

- `apic: timer: TSC-deadline LAPIC timer (tickless)` (most CPUs since 2013) or `one-shot`.
- `power: idle states (CPUID): poll, C1 (mwait), C1E (mwait 0x01), C2 (mwait 0x10), …` or `(ACPI _CST)` with `acpi: _CST:` lines giving each state's register and latency; `(hlt)` means `mwait` wasn't used.
- `power: frequency: Intel HWP …`, `AMD CPPC …`, `ACPI P-states …` or `no frequency control`, and `power: on battery` / `on AC power` when the charger is unplugged and replugged.

Then check:

- [ ] **Quiet when idle.** Leave the desktop alone for a minute and run `cpuinfo`. Each CPU's line counts its timer interrupts since boot: idle, they should grow by a few per second, not a thousand. The `idle:` line shows the time in each state: a laptop should spend most of it in the deepest one. `Wakers:` lists what woke the system most.
- [ ] **Frequency.** With HWP or CPPC, `cpuinfo` shows `~N MHz` per CPU (the average while busy). Compare Battery saver and Performance in Settings → Battery under load (say, `fetch` a large file or play a video).
- [ ] **Battery life and fan.** Compared with the same machine on another OS, idle and on battery.
- [ ] **No stalls.** The clock ticks over each minute, typing and the pointer stay smooth, sleep and wake work. If the system stutters or hangs only after the desktop appears, try `timer=periodic`, then `idle=halt`, and report which helped.

## Reporting what happened

- **The boot log.** A few seconds after the desktop appears, the kernel writes `\aurora\boot.log` to the EFI partition. It contains the CPU, the PCI and USB devices, and the full kernel log. Read it on another computer, or in Terminal:
  - `cat /Boot/aurora/boot.log`
  - `dmesg` for the live log
  - `lspci`, `lsusb`, `cpuinfo` and `battery` for the individual reports
- **A crash screen** shows the fault and where it happened. Please photograph it.
- **A hang before the desktop.** If the machine hangs early, try `acpi=off`. If it then boots, the problem is in AML: include the `aml:` lines from the log.

## Known limits

- **Graphics.** On Intel graphics from Skylake to Raptor Lake a native display driver (untested on real hardware, see [above](#intel-graphics)) keeps the firmware's mode and adds a hardware pointer, EDID, hotplug detection and backlight control. Elsewhere WaveOS draws into the framebuffer the firmware set up (GOP). Either way the resolution changes only at boot (the Intel driver's `intel=modeset` aside). Several displays work in virtual machines (virtio-gpu with several scanouts); on real hardware they need a native display driver, and nothing of multi-monitor support (hotplug, per-display modes and scales, a separate presenting task per display, EDID identities for saved arrangements) has run on a real GPU or monitor. Two QEMU std VGAs (`secondary-vga`) are not driven together: only the first is used. After sleep the screen may stay dark on some machines, because the firmware doesn't re-initialize the display; the rest of the system resumes. In virtual machines, Bochs/QEMU std VGA and virtio-gpu (WaveOS's own driver, with a hardware cursor and the window size followed live) change resolution at once and restore the picture after sleep.
- **The Linux DRM device** (`/dev/dri/card0`) is tested in QEMU on Bochs VGA, the native virtio-gpu driver and the ported Linux one (`gpu=linux`). Untested: a card on the firmware framebuffer alone (`simpledrm`; QEMU always has Bochs or virtio), devices with real vblank interrupts (no driver has them yet: vblanks are counted from the mode's refresh rate), and more than one card.
- **Programs' buffers on the screen** (windows showing a Linux program's dma-bufs or shared memory as they are, and full-screen programs scanned out directly) are tested in QEMU on Bochs VGA and virtio-gpu only. Untested: direct scanout on the Intel and AMD drivers (a driver that can't scan a program's buffer out refuses the commit, and the window is composed instead), frame times taken from real vblank interrupts, and scanning out on a display other than the first while others compose.
- **Storage.** AHCI, NVMe and virtio disks, SD cards and eMMC work, but Intel RST / RAID mode does not. USB storage and SD cards must be FAT32 (exFAT and NTFS aren't supported, and most SD cards above 32 GB come formatted exFAT). Realtek and Alcor PCIe card readers aren't supported (see [SD cards and eMMC](#sd-cards-and-emmc)). Installing to eMMC works the same as to any disk; booting from it needs firmware that can read it, as tablets' and laptops' firmware does (QEMU's OVMF can't, so the tests start the bootloader from a copy on another disk).
- **Input.**
  - Touchpads and touch screens that connect over I²C (common on laptops since about 2016) have a driver that has not yet run on real hardware (see [above](#i2c-touchpads-and-touch-screens)); on most Intel laptops it polls the touchpad, as there is no GPIO driver yet. Many such touchpads also have a PS/2 fallback, which works as a plain pointer. I²C keyboards and pens aren't supported. USB Precision Touchpads get the full gestures.
  - Bluetooth keyboards, mice and controllers aren't supported.
  - Linux programs' view of input (`/dev/input`, `/dev/hidraw*`, see [developer/linux-input.md](developer/linux-input.md)) has been exercised in QEMU only, with the PS/2 keyboard, QEMU's USB keyboard and tablet, the keyboard-played controller, and a touchpad and a raw HID controller made in software. **Untested:** real Xbox, PlayStation and Switch Pro controllers through `/dev/input` (their codes follow the Linux drivers', checked by reading, not against SDL on hardware), raw HID reports of real controllers (SDL's and Steam's HIDAPI drivers: feature reports and output reports go to the device as `GET_REPORT`/`SET_REPORT` or its OUT endpoint), real USB and I²C touchpads' multi-touch events, PS/2 and real mice's motion events, and the Caps Lock light written by a program on real keyboards. I²C HID devices have no `/dev/hidraw*` node. The USB HID driver's new paths (Linux devices for USB keyboards, mice, touchpads and controllers, raw reports for `/dev/hidraw*`, the controllers' USB location in sysfs) have not run with the virtual USB devices either: the self-tests that exercise them need QEMU's `usb-redir`, which the development host's QEMU lacks (they are skipped there); only QEMU's own USB keyboard and tablet went through them.
- **USB.**
  - xHCI (USB 3), EHCI (USB 2) with its OHCI or UHCI companions, and OHCI and UHCI on their own (USB 1.1) are all driven. EHCI, OHCI and UHCI have only been tested in QEMU.
  - Isochronous transfers (USB audio) work on xHCI only; on EHCI, OHCI and UHCI, `lsusb` shows audio devices as unsupported. No webcams yet.
- **Sound.** Intel HD Audio output (no microphone or HDMI audio yet), and USB Audio Class 1 and 2 speakers, headsets, DACs and microphones on xHCI. USB Audio 3.0-only devices aren't supported (most also offer UAC 1 or 2).
- **Networking.** Wired Ethernet only: PCI cards (see [Wired network adapters](#wired-network-adapters)), and USB adapters, docks and Android USB tethering (see [USB network adapters](#usb-network-adapters); the vendor-mode ASIX drivers and the NCM and EEM ones are untested on real hardware, iPhone tethering isn't supported). Wi-Fi: Intel adapters from the Wireless 7265 to the Wi-Fi 6E AX211 are recognized, but WaveOS can't start their radio yet, so no real Wi-Fi works. IPv4 only, with the address from DHCP.
- **System updates** (Software Update, see [developer/store-and-updates.md](developer/store-and-updates.md)) have run only in QEMU with OVMF. Untested on real firmware: the bootloader reading the home volume through the firmware's Block I/O protocol (it picks the AuroraFS partition on the boot disk by device path), its writes to `\aurora\boot.conf` before each trial start, and a new `BOOTX64.EFI` replacing the old one after a good start. Disks the firmware can't read (an SD card or eMMC on many machines, as in QEMU) can't start an update from slot a or b: the bootloader then starts the EFI partition's own system and reports the update as failed. An EFI partition too small for the system image as it is gets it compressed (`system.tar.zlib`), which takes a moment longer to start.
- **App sandbox.** The kernel's checks are tested in QEMU from inside sandboxed native and Linux programs and fuzzers, but the window server's privacy prompt, open and save panel and the menu bar's microphone dot have not been driven by the tests (they answer for the person instead) and have not run on real hardware; the dot has not been seen with a real microphone. Camera and raw USB have no interfaces yet, so those permissions guard nothing so far; input monitoring (the Linux input devices) and screen capture (other programs' DRM framebuffers) are tested in QEMU only.
- **Durability on real disks.** AuroraFS's crash safety rests on each disk's cache flush (see [developer/aurorafs.md](developer/aurorafs.md)): AHCI `FLUSH CACHE EXT`, NVMe Flush, virtio-blk flush (now with the device's write cache on), the eMMC cache flush, USB `SYNCHRONIZE CACHE`. Power cuts have been tested only in QEMU (`cargo xtask powercut`, where QEMU keeps every write it completed) and in the host simulation (lost, reordered and torn writes between flushes). **Untested:** real SATA and NVMe drives, SD cards, eMMC and USB sticks losing power, including ones that acknowledge a flush before their cache is safe (no filesystem can protect against those), and the eMMC cache flush on a real card.
- **Swap.** The swap file (`/Security/swapfile`, made the first time memory runs short) has only been exercised on QEMU's AHCI, virtio and NVMe disks. Its I/O bypasses the filesystem, straight to the partition's sectors; it has not run on real SSDs or hard disks, and sends no TRIM for freed pages (their contents are encrypted with a per-boot key, so stale pages are noise). When the system volume is on a USB stick (booting from one), the swap file is there too: slow, and it wears the stick.
- **Sleep** is S3 (suspend to RAM). Many machines since about 2019 support only "Modern Standby" (S0ix), which WaveOS doesn't implement. On those, Sleep isn't offered.

## Wired network adapters

Every driver resets the card, reads its MAC address, runs one receive and one transmit ring, reports the link, and comes back after sleep. `lspci` names the adapters WaveOS knows, and the boot log has a line for each one it drives. "QEMU" means the kernel self-tests (DHCP, ping, UDP, TCP, a megabyte over HTTP from the host, an inbound connection, IPv6, and the network after S3 sleep) pass with `cargo xtask test --net <model>`.

| Adapter | PCI ids | Driver | Interrupts | Tested |
|---|---|---|---|---|
| virtio-net | 1af4:1000, 1041 | `virtio.rs` | MSI-X | QEMU (`--net virtio`) |
| Intel 82540EM, 82545/82546 (e1000) | 8086:100e and others | `e1000.rs` | MSI, else polled | QEMU (`--net e1000`) |
| Intel 82574L (e1000e) | 8086:10d3, 10f6 | `e1000.rs` | MSI-X | QEMU (`--net e1000e`) |
| Intel 82576 (igb) | 8086:10c9, 10e8, 1526, 150a | `igb.rs` | MSI-X (MSI, polled) | QEMU (`--net igb`), MSI-X path only |
| Intel 82580, I350, I210, I211 (igb) | 8086:150e, 1521, 1533, 157b, 1539 | `igb.rs` | MSI-X or MSI | Untested on real hardware |
| Intel I225, I226 (igc, 2.5 Gbit/s) | 8086:15f2, 15f3, 15f8, 0d9f, 125b–125d | `igb.rs` | MSI-X or MSI | Untested on real hardware |
| Realtek RTL8139 | 10ec:8139 (and D-Link, Accton clones) | `rtl8139.rs` | INTx, polled until ACPI routes it | QEMU (`--net rtl8139`) |
| Realtek RTL8168/8111/8411 | 10ec:8168, 8161, 2502, 2600 | `rtl8169.rs` | MSI-X or MSI | Untested on real hardware |
| Realtek RTL8169/8110 (PCI) | 10ec:8169, 8167; 1186:4300, 4302 | `rtl8169.rs` | INTx | Untested on real hardware |
| Realtek RTL8125 (2.5 Gbit/s) | 10ec:8125, 3000 | `rtl8169.rs` | MSI-X or MSI | Untested on real hardware |
| Intel 82579, I217, I218, I219 (chipset LAN) | 8086:1502, 153a, 15b8, 15fa and others | — | — | Recognized only: the boot log says it isn't supported |

What the untested drivers do, and what they leave out:

- **Realtek RTL8168 family and RTL8125.** Written from Realtek's register descriptions. The driver tells the chip generations apart by the revision in the transmit configuration register, takes RTL8168G and later chips out of out-of-band (management) mode, resets the MAC, powers up the PHY if firmware left it off, and uses the PHY's power-on settings. It does not apply the per-revision PHY parameter tables or PHY firmware patches that other operating systems load; most chips link without them, but a few revisions may link slower or less reliably. The RTL8125 gets a link-status read over the PHY's OCP window; its 2.5 Gbit/s mode depends on the PHY's defaults. The boot log line `rtl8169: … (chip 0x…, …)` names the revision: please include it in reports.
- **Intel igb and igc.** The 82576 path is exercised in QEMU. The other igb chips share its registers; the I225/I226 differ in their reset bit and get no PHY access (their PHY negotiates on its own). Copper ports only: fibre and SerDes variants of these chips aren't claimed.
- **Intel chipset LAN (I217/I218/I219, 82579).** Not driven. Its PHY sits outside the chipset, may be in SMBus or ultra-low-power mode shared with the management engine, and needs a semaphore-guarded wake-up sequence, plus, on the I219, a descriptor-ring flush before every reset. Doing that wrong can leave the PHY unreachable until a power cycle, so these adapters are only reported. Use a PCIe card (Intel 82574L, I210, or a Realtek RTL8111) instead.

## USB network adapters

For a laptop without a supported built-in card: a USB Ethernet adapter, a USB-C dock, or a phone's USB tethering. Each one becomes an interface as soon as it is plugged in, asks DHCP for an address once its link is up, and goes away cleanly when pulled out: connections through it fail at once, and traffic moves to another interface. Adapters with a manufacturer's MAC address are named `ethN`; links with a made-up (locally administered) address, as tethered phones and USB gadgets use, are `usbN`. With several interfaces up, the default route prefers a built-in wired card, then a USB adapter, then a tethered phone, then Wi-Fi. Settings → Network shows each one as USB Ethernet with its driver and, where the device reports it, its link speed. After sleep the adapter is found again like a new one. `lsusb` names the driver, and the System Report lists each adapter's link and how many frames travel per USB transfer.

"QEMU" means `cargo xtask test --net usb-ecm` or `--net usb-rndis` passes the network tests (DHCP, ping, UDP, TCP, a megabyte each way to the host, an inbound connection, IPv6, after S3 sleep) over QEMU's `usb-net`, with a second adapter at the same time, the first pulled out in the middle of a download and plugged back (QMP `device_del`/`device_add`), on xHCI and on EHCI (QEMU's usb-net is a full-speed device, so on an EHCI machine it runs on the UHCI companion, as a full-speed device does on a real one). "Simulated" means the self-tests' device models (`kernel/src/drivers/usb/net/sim.rs`) bring the driver up, get an address over DHCP, answer ping and echo a burst of datagrams, and are unplugged: that exercises the driver's plumbing and its framing both ways, but the models are written from the same understanding of the chips as the drivers, so **these drivers are untested on real hardware**. The wire formats are also tested and fuzzed on the host (`cargo test -p aurora-usbnet`).

| Device | Ids | Driver | How it is driven | Tested |
|---|---|---|---|---|
| CDC-ECM: most USB-C dongles and docks offer it; Linux and Android gadgets; QEMU's usb-net | interface class 02/06 | `cdc.rs` | One frame per transfer; link and speed from the interrupt endpoint's notifications | QEMU (`--net usb-ecm`; xHCI and EHCI) |
| RNDIS: Android USB tethering, Windows-first adapters | interface 02/02/ff, e0/01/03, ef/04/01 | `rndis.rs` | INITIALIZE, then queries (MAC, link speed, link state) and a packet filter; several packets per transfer when the device allows; link changes from status indications; the device's keepalives answered | QEMU (`--net usb-rndis`; xHCI and EHCI) |
| CDC-NCM: 2.5 Gbit/s adapters, many docks | interface 02/0d | `cdc.rs` | 16-bit transfer blocks (NTB16; NTB32 received too) aggregating frames both ways, up to 32 KiB, with the device's alignment rules | Simulated |
| CDC-EEM | interface 02/0c/07 | `cdc.rs` | A stream of headed frames (sentinel instead of CRC), echo requests answered; no MAC address from the device, so a random local one | Host tests only |
| Realtek RTL8152, RTL8153, RTL8155, RTL8156 | 0bda:8152, 8153, 8155, 8156 (docks keep their chip's CDC configuration under their own ids) | `cdc.rs` | **Their CDC configuration**: ECM on the RTL8152/8153, NCM on the RTL8156 where it is offered | Simulated (RTL8156's NCM configuration) |
| ASIX AX88772, AX88772A/B | 0b95:7720, 772a, 772b, 7e2b; Apple USB Ethernet Adapter 05ac:1402; D-Link DUB-E100 2001:1a02, 2001:3c05, 07d1:3c05; Linksys USB200M 13b1:0018 | `asix.rs` | Vendor mode | Simulated |
| ASIX AX88179, AX88178A | 0b95:1790, 178a; D-Link DUB-1312 2001:4a00; Sitecom 0df6:0072; Samsung 04e8:a100; Lenovo 17ef:304b; Toshiba 0930:0a13 | `asix.rs` | Vendor mode; an AX88179A that also offers CDC-NCM runs in NCM | Simulated |

Which configuration a device runs in is chosen when it is plugged in: a configuration with NCM wins, then ECM, then a vendor chip with a driver here, then EEM, then RNDIS. The boot option `usbnet=ecm`, `ncm`, `eem`, `rndis` or `vendor` prefers one where the device offers it (the boot log says `usb: … using configuration N` when it isn't the first).

What the untested drivers do, and what they leave out:

- **Realtek RTL8152/8153/8155/8156.** No vendor-mode driver: it needs Realtek's OCP register programming and per-revision PHY firmware patches, which can't be checked without the hardware. Their CDC configuration is a standard function the ECM and NCM drivers already run, so a dongle or dock that offers one works through it. In ECM mode frames are not aggregated, so expect less throughput than a vendor driver gives at gigabit speeds; ECM devices report their speed only if they send speed notifications. A Realtek device that offers no CDC configuration is listed by `lsusb` without a driver.
- **ASIX AX88772.** Follows ASIX's reference bring-up: GPIOs, the embedded PHY selected and reset, the MAC address read from the chip, 10/100 autonegotiation restarted over MII, the receiver started for our address, broadcasts and all multicast. The link comes from the interrupt endpoint; when it comes up, speed and duplex are read back from the PHY and set in the MAC. Frames arrive on a stream that runs across transfers. Not done: the chip's checksum offload, its multicast hash filter (all multicast is accepted), wake-on-LAN, external PHYs other than the PHY address the chip reports.
- **ASIX AX88179/AX88178A.** PHY power-up, clocks, MAC address, receive aggregation (bulk-in queue settings by link and bus speed), receiver on, autonegotiation restarted; on link-up, the PHY's speed and duplex go into the medium mode register. Receive transfers hold several frames with a trailer. Not done: checksum offload, the multicast filter, EEE and LED settings, wake-on-LAN. Some AX88179A firmware also offers CDC-NCM, which is preferred.
- **CDC-NCM.** NTB16 out, NTB16 and NTB32 in; no CRC mode; datagrams placed as the device's divisor and remainder ask (a nonsensical value falls back to 4-byte alignment); the input block size is set to at most 32 KiB.
- **CDC-EEM.** Only the framing is tested (on the host); no device or model has run the driver.
- **RNDIS.** Tested in QEMU only; real phones answer the same messages, and any Android phone's USB tethering is RNDIS (newer ones may offer NCM, which then wins). The multicast list OID isn't used: all multicast is accepted.

Not supported:

- **iPhone USB tethering** (Apple's `ipheth`, 05ac:12xx). It needs the device switched to the configuration with Apple's tethering interface (vendor class, subclass fd, protocol 1), alternate setting 1, a vendor request for the MAC address and carrier checks, and its own framing (2-byte padded frames; recent iOS versions use NCM-style blocks after a vendor request). More importantly, as far as we know the phone tethers only to a computer it trusts (the "Trust This Computer?" prompt), which takes Apple's lockdown pairing protocol (TLS with device certificates, as `usbmuxd` and `libimobiledevice` implement it); none of that could be checked here without an iPhone, so no driver is attempted.
- **SMSC/Microchip LAN9500/9512/9514 and LAN7800/7850** (0424:ec00, 9500, 9512, 9514, 7800, 7850; the Raspberry Pi's built-in Ethernet): vendor protocols not implemented.
- **Realtek chips without a CDC configuration**, mobile broadband modems (MBIM, QMI) and Wi-Fi dongles.

Performance (QEMU 11 under TCG on a Mac; QEMU's usb-net moves one frame per transfer, and its numbers vary from run to run): a megabyte to or from the host takes about 20–70 ms over usb-net on xHCI (15–50 MB/s), like virtio-net and faster than the emulated e1000 in the same setup, and about 2 s on EHCI (via its UHCI companion, at full speed: 0.5 MB/s each way). See [the developer notes](developer/usb-networking.md) for the measurements and where the time goes.

## 64-bit Arm

The aarch64 port runs only on QEMU's `virt` machine so far (HVF on Apple
silicon, TCG elsewhere). Untested anywhere else: real Arm hardware (other
UEFI firmware, GICv2 systems, non-coherent DMA, PL011 consoles at other
addresses than `virt`'s, PSCI through `smc` on hardware, firmware entering
at EL2 with E2H forced on, GICv4 redistributor layouts, more than one
GICv2m frame or an ITS, big.LITTLE CPUs with differing ID registers).

What has run there, under HVF on an Apple M2: the kernel self-tests on
both boots, the scripted desktop session (`tools/smoke_desktop.py`), every
native aarch64 program, the arm64 Linux programs (BusyBox, wl-bridge, the
C tests, Doom), the arm64 Debian root with Mesa, Voyage and Firefox, and
video with sound in both browsers (`cargo xtask video-check`). The Linux
layer's arm64 ABI is also checked on the host against Debian's arm64
headers (`cargo test -p aurora-linuxabi`). Not there: suspend to RAM, the
simulated battery, the ported Linux GPU driver, NetSurf and libdrm's
`modetest` (x86-64 only so far). Under emulation (TCG) the aarch64 suite
was not rerun after the latest changes. The x86 and aarch64 disks'
architecture checks, `migrate-disk`, the per-architecture catalogues and
the slot check in the bootloader are tested on the host only (image and
repository tests; `cargo test -p xtask -p gina`). On QEMU 8.2 (Ubuntu
24.04), which can't choose the `virt` machine's MSI controller, the guest
gets an ITS under TCG instead of a GICv2m frame.

## Compatibility

Please add a row for each machine you try.

| Machine | CPU | Boots | Display | Keyboard / pointer | USB storage | Disk | Network | Sound | Battery | Sleep | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| QEMU 11 (q35, Homebrew OVMF), macOS host | TCG, 4 cores | ✅ | ✅ | ✅ PS/2, USB | ✅ | ✅ AHCI, NVMe, virtio, SD, eMMC | ✅ virtio-net, e1000, e1000e, igb, RTL8139 | ✅ HDA, USB audio (xHCI) | ✅ test SSDT | ✅ | Development setup (`cargo xtask run` / `test`) |
| QEMU 8.2 (q35, Ubuntu OVMF), Linux host | TCG, 4 cores | ✅ | ✅ | ✅ PS/2, USB on xHCI, EHCI+UHCI, EHCI+OHCI, OHCI, UHCI; virtual touchpad, keyboard and controllers over usb-redir | ✅ every controller | ✅ AHCI | ✅ virtio-net | ✅ HDA | ✅ test SSDT | ⚠️ firmware doesn't resume | `cargo xtask test --usb …` |
| QEMU and OVMF from Ubuntu 24.04 | TCG, 4 cores | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | CI. This firmware build faults while resuming from S3, before WaveOS runs, so the tests skip sleep there |
| | | | | | | | | | | | |
