# Storage and persistence
AuroraFS is the journaled home filesystem. The existing on-disk signature remains compatible with earlier WaveOS images. /System is the read-only application image; /Boot exposes the FAT boot partition; removable volumes appear under /Volumes.

## Verify persistence
1. In Terminal, run `echo persistence > /Documents/check.txt`.
2. Run `sync`, then restart from the Aurora menu.
3. Open `/Documents/check.txt` in Notes or use `cat`.

The same text should remain on a writable AuroraFS disk. An ISO without a writable home volume uses temporary storage.

## Safe operations
Use Files to eject removable storage before disconnecting it. Shut down through the OS so cached writes are flushed. Keep host-side disk backups while the VM is stopped; never copy an actively changing raw image as your only backup.

## Troubleshooting
Use `df` to inspect space. A full volume can prevent document, setting, or package writes. Do not format an unknown partition to fix a mount failure. Restore a known-good image copy and preserve the original for investigation.
