# WaveOS Aurora 0.8.0

Not yet published.

WaveOS has a new look, **living water**: glass bubbles along the top, Crest in the middle, Harbor floating at the bottom, and windows without title bars. It starts with the WayV logo, asks for your password, and every surface and app has been redrawn in light and dark. This update also includes every change listed for 0.7.1.

## Starting up
- A boot screen shows the WayV logo, progress through each start-up stage and the version, in the Light or Dark appearance you chose.
- **Shore** sets up your account on the first start: your name, a password and a hint, then the appearance and wallpaper. The password is salted and hashed (PBKDF2-HMAC-SHA256), never stored.
- After that, Shore asks for the password at start-up, after **Lock Screen** (Super L, or the WayV menu) and after sleep. **Settings → Login** can log you in automatically at start-up.

## The top bar
- The left bubble holds the WayV menu and the app you're using; the right bubble holds **Seek** and the status ring.
- **Crest**, in the middle, shows the time. Point at it for the date; it deepens to show the volume, power changes and new notifications. Click it for **Drift**.
- The status ring opens **Current**: the network, Calm, the appearance, Lock, the sound and the battery.
- **Settings → Bar & Crest** turns Crest off (the time and date move into the right bubble) and shows the battery level next to the ring.

## Harbor
- Harbor replaces the dock. Three bays hold Shoal and your pinned apps, other open apps, and the Downloads and Documents stacks with the Trash.
- A waterline glows under open apps, widest for the one you're using. Pointing lifts and names an icon; resting on an open app shows its windows, and clicking one brings it forward.
- Harbor sinks away when the front window reaches it, and rises when the pointer touches the bottom of the screen.

## Windows
- Windows no longer have title bars. Bring the pointer to a window's top edge and its handle rises into a pill: drag it to move the window, double-click it to expand, or use its buttons to shrink, expand or close.
- Hold Super and drag anywhere in a window to move it.
- Drag a window to the top of the screen and **Moorings** offers six layouts — halves, thirds and quarters. The focused window glows with a waterline.

## Shoal, Seek, Drift and Glide
- **Shoal** shows every app full screen, with search and groups (Work, Create, System, Installed). Press and release Super, or click the first icon in Harbor.
- **Seek** (Super+Space) finds apps, files, settings and sums.
- **Drift** drops from Crest with the date, a calendar, Calm and your notifications.
- **Glide** (Alt+Tab) shows each window as a picture.

## Apps
- **About This Computer** shows the WVOS mark, the version and your machine, made by WayV. Settings → About has a WVOS card.
- Apps take new names: **Lens** (was Preview), **Pulse** (was Activity Monitor), **Installer** (was GINA Apps) and **Studio** (was Constellation Studio). Do Not Disturb is now **Calm**.
- Manrope is the interface typeface. New icons for every app, and three new wallpapers — Tide, Aurora and Dunes — each with a day and a night picture. The accent colour can follow the wallpaper.

## Known limitations
WaveOS has no Wi-Fi or Bluetooth driver, so Current shows the wired network only. There is one account per computer. The frame-time numbers recorded during development came from software emulation, so this update makes no performance claim. Read the [complete handbook](../../handbook/0.7/en/start-welcome.md); it still shows the 0.7 desktop.

## Evidence
`cargo xtask lint`, the host test suite in [Building and testing](../../handbook/0.7/en/developer-build.md), and the kernel self-tests (`cargo xtask test`, both boots, including the new password-hash test) were run on the change. Each part of the new desktop was exercised in QEMU at 1280 × 800; the screenshots in `docs/screenshots/0.8` come from those runs.
