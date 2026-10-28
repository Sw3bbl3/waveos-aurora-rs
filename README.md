<div align="center">

# WaveOS Aurora

### An entire operating system. Written from scratch. In Rust.

The bootloader, kernel, drivers, filesystem, window server, network stack, web browser, and every app — designed together, built together, from the first instruction to the last pixel.

![WaveOS Aurora in dark mode: Lens and Notes moored side by side, with Harbor risen to peek at Notes](docs/screenshots/0.8/desktop-dark.png)

[Get started](#get-started) · [Explore the system](docs/explorer/index.html) · [Read the handbook](docs/handbook/0.7/en/start-welcome.md) · [Every feature](docs/FEATURES.md)

</div>

---

## Beautiful by default.

**Living water**, the WaveOS look: glass bubbles along the top, **Crest** in the middle with the time and whatever is happening now, and **Harbor** floating at the bottom with your apps in three bays. Windows have no title bars: bring the pointer to the top edge and a handle rises to move, shrink, expand or close them. Light and dark everywhere, an accent colour that can follow your wallpaper, and every word set in Manrope by **Lyra**, our own TrueType engine.

| | |
|---|---|
| ![The boot screen: the WayV logo, a progress bar and the version](docs/screenshots/0.8/boot.png) | ![Shore, the login screen, with the date, a large clock and the password field](docs/screenshots/0.8/shore-login.png) |
| **Start-up.** The WayV logo and real progress, in your appearance. | **Shore.** One account, a salted and hashed password, and Lock Screen with Super L. |
| ![Shoal, every app full screen, with search and groups](docs/screenshots/0.8/shoal.png) | ![Seek finding a picture](docs/screenshots/0.8/seek.png) |
| **Shoal.** Every app, full screen. Type to narrow it down. | **Seek.** Apps, files, settings and quick maths, one keystroke away. |
| ![Drift, dropped from Crest: the calendar, Calm and notifications](docs/screenshots/0.8/drift.png) | ![Current, opened from the status ring: network, Calm, appearance, lock and sound](docs/screenshots/0.8/current.png) |
| **Drift.** Click Crest for the calendar, Calm and your notifications. | **Current.** The status ring opens the network, Calm, appearance and sound. |
| ![Glide, Alt+Tab with a picture of each window](docs/screenshots/0.8/glide.png) | ![Moorings, the layouts that appear when a window is dragged to the top](docs/screenshots/0.8/moorings.png) |
| **Glide.** Alt+Tab shows each window as it is. | **Moorings.** Drag a window to the top and drop it into a layout. |
| ![The desktop in the light appearance](docs/screenshots/0.8/desktop-light.png) | ![Settings, Bar & Crest, with a live preview of the top bar](docs/screenshots/0.8/settings-bar-crest.png) |
| **Light, too.** Every surface, every app. | **Settings.** Turn Crest off, show the battery level, log in automatically. |

## The web, on an engine of our own.

**Nebula** loads real websites through a stack we wrote ourselves: the network drivers, TCP/IP, TLS 1.3, HTTP, and an HTML and CSS engine with flexbox, grid and tables.

| | |
|---|---|
| ![Nebula showing Wikipedia's main page, with photos](docs/screenshots/surf-wikipedia.png) | ![Nebula showing Hacker News](docs/screenshots/surf-hacker-news.png) |

## JavaScript, written from scratch too.

**Pulsar** is our own JavaScript engine: a compiler, a virtual machine, a garbage collector, and core standard-library facilities, from classes and generators to `async`/`await` and regular expressions. It passes more than 34,000 of the official conformance tests. In Nebula it brings pages to life: the DOM, events, timers, `fetch`, storage, and libraries like jQuery. In Terminal, type `js` and start writing.

| | |
|---|---|
| ![The Pulsar demo page in Nebula: a counter, a to-do list and a live clock](docs/screenshots/pulsar-demo.png) | ![The js prompt in Terminal, with a class, JSON and a loop](docs/screenshots/pulsar-repl.png) |
| **In Nebula.** Open *Pulsar demo* from the start page. | **In Terminal.** `js` for a prompt, `js file.js` to run a file. |

## Every app in its own world.

Each app runs as a separate, protected process. If one crashes, you see a polite "quit unexpectedly" — and everything else keeps right on working. Your files live on **AuroraFS**, a journaled filesystem built to survive a power cut.

| | |
|---|---|
| ![Files showing a USB stick under Locations, with the battery popover open](docs/screenshots/usb-and-battery.png) | ![Pulse (then called Activity Monitor), CPU tab](docs/screenshots/activity-monitor.png) |
| **Files.** Drag and drop, Quick Look, thumbnails, a Trash with Put Back, and USB sticks that just appear. | **Pulse.** Every process, every core, every byte — live. |

## Made for real machines.

Multicore scheduling. USB 3. NVMe and SATA. Sound with headphone detection. Battery, lid and sleep. Wired networking that's ready the moment you boot.

| | |
|---|---|
| ![About This Computer: the WVOS mark, the version and Made by WayV](docs/screenshots/0.8/about-light.png) | ![Terminal running ping, nslookup and fetch over HTTPS](docs/screenshots/network-tools.png) |
| **About This Computer.** The WVOS mark, the version and the machine. | **Terminal.** A real shell, with pipes, redirection and 35 programs. |

## See inside, while it runs.

The **System Explorer** is a live map of the whole OS. Watch it boot, see each CPU's scheduling decisions, and click any part to see what it's doing and the source code behind it.

![Aurora System Explorer](docs/screenshots/explorer.png)

---

## Get started

You need a Mac (Apple silicon or Intel) or a Linux PC, [rustup](https://rustup.rs) and QEMU (`brew install qemu`, or `sudo apt install qemu-system-x86 ovmf`).

```sh
git clone https://github.com/Sw3bbl3/waveos-aurora-rs.git
cd waveos-aurora-rs
cargo xtask run
```

A minute later, you're at the desktop. Add `--monitor` to open the System Explorer alongside it. To try a real PC, see [HARDWARE.md](docs/HARDWARE.md); for every build command, see [FEATURES.md](docs/FEATURES.md#build-commands).

## Under the hood

| | |
|---|---|
| **Firstlight** | UEFI bootloader |
| **Aster** | Hybrid kernel: multicore, ACPI, sleep and resume |
| **AuroraFS** | Journaled, extent-based filesystem |
| **Lumen Server** | Compositing window server |
| **WaveUI** | Declarative interface framework every app is built with ([WaveSDK](docs/WAVESDK.md)) |
| **AuroraKit** | Windowing toolkit WaveUI runs on |
| **Lyra** | TrueType font engine |
| **Nebula** | Web browser: HTTP, TLS, HTML, CSS and layout |
| **Pulsar** | JavaScript engine: compiler, VM, collector, DOM — [status](docs/PULSAR.md) |

Nearly everything is our own. Where we stand on others' shoulders, we say so: cryptographic primitives come from [RustCrypto](https://github.com/RustCrypto), ACPI's AML interpreter from the [`acpi`](https://crates.io/crates/acpi) crate, and maths functions from [`libm`](https://crates.io/crates/libm).

Go deeper: [Architecture](docs/ARCHITECTURE.md) · [Every feature](docs/FEATURES.md) · [Roadmap](docs/ROADMAP.md) · [Building and debugging](docs/BUILDING.md) · [Validation](docs/VALIDATION-2026-09-30.md) · [Contributing](CONTRIBUTING.md)

## License

Code under the [MIT License](LICENSE). Manrope and JetBrains Mono under the SIL Open Font License.

Native H.264/AAC media decoding uses licensed third-party Rust components. See [third-party notices](THIRD_PARTY_NOTICES.md) for attribution and fixture provenance.
