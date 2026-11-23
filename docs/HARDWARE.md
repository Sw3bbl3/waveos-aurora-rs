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
| `gpu=linux` | Drive a virtio GPU with the ported Linux driver instead of the native one (virtual machines) |
| `cursor=software` | Draw the pointer into the picture instead of on the display's cursor plane (on Intel graphics this also leaves the display buffer as the firmware set it up) |
| `intel=off` | Leave Intel graphics to the firmware framebuffer, as before the Intel driver existed |
| `intel=modeset` | Let the Intel driver change resolutions (Skylake, Kaby, Coffee and Comet Lake, external displays only, no larger than the firmware's mode; untested) |
| `timer=periodic` | Go back to a 1 kHz tick on every CPU (instead of tickless timers). `timer=oneshot` avoids the TSC-deadline timer. Try these if the machine hangs or stutters once the desktop is up. |
| `idle=halt` | Idle with `hlt` only (no `mwait` C-states). `idle=mwait` uses `mwait` even in a virtual machine; `idle=poll` never sleeps (for measurements). |

## What to check

Work through the list and note what happens. The boot log (below) records the details.

- [ ] **Boots to the desktop.** Note the time it takes and whether the picture looks right.
- [ ] **Display.** Check the resolution, and whether the text is sharp. On Intel graphics, see [Intel graphics](#intel-graphics) below.
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

- **Graphics.** On Intel graphics from Skylake to Raptor Lake a native display driver (untested on real hardware, see [above](#intel-graphics)) keeps the firmware's mode and adds a hardware pointer, EDID, hotplug detection and backlight control. Elsewhere WaveOS draws into the framebuffer the firmware set up (GOP). Either way the resolution changes only at boot (the Intel driver's `intel=modeset` aside). After sleep the screen may stay dark on some machines, because the firmware doesn't re-initialize the display; the rest of the system resumes. In virtual machines, Bochs/QEMU std VGA and virtio-gpu (WaveOS's own driver, with a hardware cursor and the window size followed live) change resolution at once and restore the picture after sleep.
- **The Linux DRM device** (`/dev/dri/card0`) is tested in QEMU on Bochs VGA, the native virtio-gpu driver and the ported Linux one (`gpu=linux`). Untested: a card on the firmware framebuffer alone (`simpledrm`; QEMU always has Bochs or virtio), devices with real vblank interrupts (no driver has them yet: vblanks are counted from the mode's refresh rate), and more than one card.
- **Storage.** AHCI, NVMe and virtio disks work, but Intel RST / RAID mode does not. USB storage must be FAT32 (exFAT and NTFS aren't supported).
- **Input.**
  - Touchpads and touch screens that connect over I²C (common on laptops since about 2016) have a driver that has not yet run on real hardware (see [above](#i2c-touchpads-and-touch-screens)); on most Intel laptops it polls the touchpad, as there is no GPIO driver yet. Many such touchpads also have a PS/2 fallback, which works as a plain pointer. I²C keyboards and pens aren't supported. USB Precision Touchpads get the full gestures.
  - Bluetooth keyboards, mice and controllers aren't supported.
- **USB.**
  - xHCI (USB 3), EHCI (USB 2) with its OHCI or UHCI companions, and OHCI and UHCI on their own (USB 1.1) are all driven. EHCI, OHCI and UHCI have only been tested in QEMU.
  - Isochronous transfers (USB audio) work on xHCI only; on EHCI, OHCI and UHCI, `lsusb` shows audio devices as unsupported. No webcams yet.
- **Sound.** Intel HD Audio output (no microphone or HDMI audio yet), and USB Audio Class 1 and 2 speakers, headsets, DACs and microphones on xHCI. USB Audio 3.0-only devices aren't supported (most also offer UAC 1 or 2).
- **Networking.** Wired Ethernet only; see [Wired network adapters](#wired-network-adapters) for which chips are driven and how far each is tested. Wi-Fi: Intel adapters from the Wireless 7265 to the Wi-Fi 6E AX211 are recognized, but WaveOS can't start their radio yet, so no real Wi-Fi works. IPv4 only, with the address from DHCP.
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

## Compatibility

Please add a row for each machine you try.

| Machine | CPU | Boots | Display | Keyboard / pointer | USB storage | Disk | Network | Sound | Battery | Sleep | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| QEMU 11 (q35, Homebrew OVMF), macOS host | TCG, 4 cores | ✅ | ✅ | ✅ PS/2, USB | ✅ | ✅ AHCI, NVMe, virtio | ✅ virtio-net, e1000, e1000e, igb, RTL8139 | ✅ HDA, USB audio (xHCI) | ✅ test SSDT | ✅ | Development setup (`cargo xtask run` / `test`) |
| QEMU 8.2 (q35, Ubuntu OVMF), Linux host | TCG, 4 cores | ✅ | ✅ | ✅ PS/2, USB on xHCI, EHCI+UHCI, EHCI+OHCI, OHCI, UHCI; virtual touchpad, keyboard and controllers over usb-redir | ✅ every controller | ✅ AHCI | ✅ virtio-net | ✅ HDA | ✅ test SSDT | ⚠️ firmware doesn't resume | `cargo xtask test --usb …` |
| QEMU and OVMF from Ubuntu 24.04 | TCG, 4 cores | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | CI. This firmware build faults while resuming from S3, before WaveOS runs, so the tests skip sleep there |
| | | | | | | | | | | | |
