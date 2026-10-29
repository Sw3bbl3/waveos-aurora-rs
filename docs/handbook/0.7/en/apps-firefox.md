# Firefox
Browse the web with full JavaScript.

## Use the app
1. Open Firefox from the dock, the launcher or Seek. Web links in Learn and About and `.html` files open in it too.
2. Press Ctrl+L, type an address or a search and press Enter. Ctrl+T opens a tab and Ctrl+W closes it.
3. Use Alt+Left/Right or a mouse's side buttons to go back and forward, Ctrl+R to reload and Ctrl+F to find on the page. Ctrl+plus/minus/0 change the zoom.
4. Downloads go to your Downloads folder, where Files shows them.

## Expected result and troubleshooting
Firefox is Mozilla's Firefox ESR, from Debian, running on WaveOS's Linux layer: it shows sites as Firefox does elsewhere, with JavaScript, video and modern layout. It follows WaveOS's light or dark appearance when it starts. It takes about half a minute to start in a virtual machine that emulates the processor (as on Macs with Apple silicon), much less where the virtual machine uses the processor's own virtualization; later pages and tabs are quicker.

Firefox comes with the Linux packages on the disk. If they aren't there (the disk was made without them, or they couldn't be downloaded), NetSurf opens instead; see [NetSurf](apps-netsurf.md). Firefox's sandboxes are off on WaveOS, which doesn't have the Linux features they need yet: browse sites you trust.

See [Desktop and windows](desktop-windows.md) for shared shortcuts and [Recovery](reference-recovery.md) if the app cannot open.
