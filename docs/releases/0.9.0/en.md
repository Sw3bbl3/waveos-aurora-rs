# WaveOS Aurora 0.9.0

Not yet published.

WaveOS draws faster, shows your battery, runs its first graphics driver from Linux, and has Wi-Fi in Settings. This update includes every change listed for 0.8.0.

## Faster
- The desktop's drawing takes less than half the time it did: window shadows, translucent fills, borders and glass are all quicker, and the login screen's background blur is about six times faster (measured on the host CPU; the data is in `docs/measurements/drawing-2026-10-27` in the source code).
- The window server draws with SSE2 in its own task, sends only the parts of the screen that changed when you drag a window, and maps the screen write-combining, so frames reach the display in bursts.
- The blinking caret on the login screen no longer repaints the whole screen.
- Pictures (JPEG) decode faster, with identical results.

## Batteries
- Laptops' batteries and power adapters read correctly on more machines: two bugs in the ACPI interpreter that made some batteries read 0 %, or stopped the battery code altogether, are fixed.
- In a virtual machine, `cargo xtask run --battery=host` shows this computer's own battery in WaveOS, live, on Linux, macOS and Windows hosts.

## Graphics: drivers from Linux
- WaveOS can now run graphics drivers written for Linux. The first is Linux's own **virtio-gpu** driver with the DRM display core, unmodified, on a compatibility layer written for WaveOS. In a virtual machine with a virtio display (`cargo xtask run --gpu virtio`), it sets the display mode and shows the whole desktop.
- Only Linux code whose license allows it is included (MIT and similar). Where Linux's implementation is GPL-only, WaveOS has its own.

![The login screen, shown by the Linux virtio-gpu driver](../../screenshots/0.9/virtio-gpu-login.png)

## Wi-Fi
- **Settings → Network** has a Wi-Fi section: the networks around you, a password field, **Join** and **Disconnect**. WaveOS joins open and WPA2-Personal networks; WEP, WPA3-only and enterprise networks are listed but can't be joined yet.
- When you're on Wi-Fi, the status ring shows the Wi-Fi symbol and Current shows the network's name.
- Intel Wi-Fi adapters (Wireless 7265 to Wi-Fi 6E AX211) are recognized, but WaveOS can't start their radio yet; Settings says so. Wi-Fi works today on the simulated networks for testing (`cargo xtask run --wifi-sim`, network "Harbor Cafe", password "tide pools at dawn").

![Joining a network](../../screenshots/0.9/wifi-password.png)

## For developers
- `cargo xtask run --gpu virtio` boots with a virtio display driven by the Linux driver; `cargo xtask test --gpu virtio` also checks that the display came up.
- `cargo xtask run --wifi-sim` adds the simulated Wi-Fi networks.
- Building the graphics drivers needs clang. Without it, the kernel builds without them.

## Known limitations
No real Wi-Fi adapter works yet: the Intel driver stops before starting the radio, and none of it has run on real hardware. The Linux graphics driver works in virtual machines (virtio); drivers for AMD, Intel and NVIDIA graphics, and 3D acceleration, are not there yet. In this update's test machine the firmware did not wake from sleep, so the sleep test was skipped; it was skipped the same way on 0.8.0's code.

## Evidence
`cargo xtask lint`, the host tests in [Building and testing](../../handbook/0.7/en/developer-build.md) (now including the Wi-Fi library's tests against the IEEE 802.11 test vectors and an independent implementation), and the kernel self-tests on both boots (37 tests, including the Linux compatibility layer's self-test and a WPA2 connection with DHCP on the simulated network) were run on the change, as was `cargo xtask test --gpu virtio`. The screenshots in `docs/screenshots/0.9` come from QEMU at 1280 × 800.
