# Troubleshooting and recovery
Start with the symptom and preserve evidence before changing a disk.

## Boot or display failure
1. Confirm the VM uses x86-64 UEFI and the expected release image.
2. Capture the serial log from `cargo xtask run`.
3. If a new display mode is unreadable, let the confirmation expire or choose Revert.
4. Restart using a known-good disk copy if necessary. Keep the failing image for diagnosis.

## Input problems on macOS
Enable QEMU in System Settings → Privacy & Security → Accessibility, restart QEMU, and click inside the VM. Control+Option+G releases capture. Command interception must be checked with a physical keyboard; absence of a warning is not enough.

## Application problems
An isolated process crash should leave the desktop usable. Reopen the app, check the crash report and `dmesg`, and reproduce with a small document. If Learn reports missing content, rebuild the system archive with `cargo xtask docs` and `cargo xtask build`.

## Missing files or settings
Use `df` to confirm a writable AuroraFS home volume. ISO-only sessions may use temporary storage. Shut down cleanly and verify the host is reopening the same persistent image. Restore backups while the VM is stopped; never recreate a disk before recovering files.

## Report a problem
Include release version, host architecture, QEMU version, VM options, reproduction steps, expected/actual result, serial log, and a screenshot. Remove private document contents from public reports.
