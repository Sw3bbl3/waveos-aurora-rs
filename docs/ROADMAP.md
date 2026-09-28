# Roadmap

WaveOS Aurora grows in milestones. Each one ends with something you can boot and use.

## ✅ M1: Boot to a graphical desktop

- The UEFI bootloader, and a higher-half kernel with physical memory management and a heap.
- ACPI, APIC timer and IRQ routing, a preemptive scheduler, PS/2 input, the RTC, and ACPI power off and restart.
- The Crest compositor and the Aurora desktop: menu bar, dock, launcher, windows, light and dark themes, and wallpapers.
- The apps: Files, Terminal, Notes, Calculator, Settings, About and Welcome, all on an in-memory RamFS.
- A kernel self-test suite, headless CI boots, and a USB-bootable GPT image.

## M2: User space

- **Protection:** ring 3 and a per-process address space. Use the upper half for the kernel and the lower half for user space, with guard pages.
- **System calls:** a `syscall`/`sysret` entry, a syscall table, and an ELF loader for user programs.
- **IPC:** ports, message passing and shared-memory surfaces, which is the hybrid-kernel core.
- **Window server:** Crest moves into a user-space window server, as macOS's WindowServer does. Apps draw into shared buffers.
- **Toolkit:** "Ripple", a UI toolkit library, provides the controls and theming apps use today, for use by user programs.
- **App migration:** Terminal becomes a real shell process, and the built-in apps become separate programs.

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
