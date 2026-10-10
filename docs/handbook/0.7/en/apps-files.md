# Files
Manage documents and mounted volumes.

## Use the app
1. Open Files and choose Documents, Pictures, or a location in the sidebar.
2. Use New Folder, F2 to rename, and Ctrl+C/Ctrl+X followed by Ctrl+V to copy or move.
3. Use Ctrl+Shift+1/Ctrl+Shift+2 for icon/list view and Ctrl+F to search. Press Space for Quick Look.
4. Delete moves an item to Trash. Open Trash and use Put Back to restore it.

## Expected result and troubleshooting
Drag between windows or onto folders; hold Ctrl to copy. Empty Trash permanently removes items. If a USB volume is absent, inspect `lsusb` and use a supported filesystem. /System is read-only.

See [Desktop and windows](desktop-windows.md) for shared shortcuts and [Recovery](reference-recovery.md) if the app cannot open.

## Tabs and transfers
Use Ctrl+T for a new tab, Ctrl+Tab (or Ctrl+Shift+Tab) to switch, Ctrl+1–8 to select a tab, and Ctrl+W to close it. Up to eight tabs keep separate locations, selection, search, history, and scroll positions for the current session.

Copying, moving, and duplicating run in the background. The transfer panel shows the current item, bytes copied, completed items, and errors. One transfer runs per window; browsing and tab switching remain available. Ctrl+J focuses transfer controls.

For a name conflict, choose Replace, Keep Both, or Skip. Replace is available for different regular files; folders and copying an item onto itself offer Keep Both or Skip. Apply all remembers your choice for remaining compatible conflicts in this transfer. With the panel focused, use Tab/Enter or R/K/S/A; Esc cancels.

Cancel keeps completed items and removes this operation's temporary copies. Closing the window offers Keep Open or Cancel & Close and waits for cleanup. Errors remain available in the result panel; Next error cycles through them. An interrupted power supply can leave hidden `.waveos-transfer-*` staging folders; do not remove one while a transfer is running. Files does not promise transactional replacement on every mounted filesystem.
