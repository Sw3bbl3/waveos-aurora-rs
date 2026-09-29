# Roadmap

WaveOS Aurora grows in milestones. Each one ends with something you can boot and use.

## ✅ M1: Boot to a graphical desktop

- The UEFI bootloader, and a higher-half kernel with physical memory management and a heap.
- ACPI, APIC timer and IRQ routing, a preemptive scheduler, PS/2 input, the RTC, and ACPI power off and restart.
- The Crest compositor and the Aurora desktop: menu bar, dock, launcher, windows, light and dark themes, and wallpapers.
- The apps: Files, Terminal, Notes, Calculator, Settings, About and Welcome, all on an in-memory RamFS.
- A kernel self-test suite, headless CI boots, and a USB-bootable GPT image.

## ✅ M2: User space

- **Ring 3:** per-process address spaces with NX, `syscall`/`sysret`, and a validated user-pointer ABI (`libs/abi`).
- **Processes:** an ELF loader and spawn/wait/kill. Kills are cooperative, and a reaper task frees exited processes.
- **IPC and isolation:** fd tables and pipes. A CPU fault in ring 3 kills only the faulting process, and the desktop shows a crash report.
- **Crest:** stays in the kernel, like NT's win32k. Apps draw into shared-memory surfaces and receive events through `next_event`.
- **Userland:** the libaurora runtime and the Ripple UI toolkit. All seven apps now run in user space.
- **Terminal:** a real shell. Programs in `/System/Bin` run as separate processes, with pipelines and redirection.
- **Tests:** the `usertest` syscall conformance suite runs as part of `cargo xtask test`.
- **Deferred:** message-port IPC and a user-space window server are planned together with multi-threaded processes.

## M3: Storage and WaveFS

- **Block drivers:** virtio-blk first, then AHCI (SATA) and NVMe.
- **VFS:** a virtual filesystem layer, a page cache, and FAT32 read/write so the ESP can be used.
- **WaveFS:** our own journaled filesystem, with extents and copy-on-write metadata.
- **Files app:** real volumes, copy, move and rename, and drag and drop.
- **Persistence:** saved settings, such as theme and wallpaper.

## M4: Apps and polish

- **Notes:** a richer editor with selection, the clipboard and undo.
- **Image Viewer:** PNG decoding.
- **Settings:** display modes, keyboard layouts, date and time.
- **Desktop:** window animations, notification center, and Spotlight-style search in the launcher.
- **Text:** runtime TrueType rendering for arbitrary sizes and scripts.

## M5: Real hardware

- **Multiprocessor:** SMP bring-up of the application processors and a per-CPU scheduler.
- **Timers:** HPET and TSC-deadline timers.
- **USB:** an xHCI host controller driver with HID keyboard and mouse.
- **Power:** proper ACPI through an AML interpreter, battery status, sleep and resume.
- **Validation:** test USB boots on several real laptops and desktops.

## M6: Networking

- **NIC drivers:** virtio-net and Intel e1000.
- **Networking stack:** TCP/IP, starting with smoltcp and moving to our own stack; then DHCP, DNS, an HTTP client, and eventually a simple web browser.
