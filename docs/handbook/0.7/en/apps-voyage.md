# Voyage
Browse the web with WebKit, the engine of Safari: full JavaScript and modern CSS.

## Use the app
1. Open Voyage from the dock, the launcher or Seek. Web links in Learn and About and `.html` files open in it too.
2. Type an address or a search in the field at the top and press Enter (Ctrl+L or F6 takes you there). Searches go to DuckDuckGo.
3. Ctrl+T opens a tab and Ctrl+W closes it; Ctrl+Tab and Ctrl+Shift+Tab move between tabs, and tabs can be dragged into another order. Links that open a new window open a tab.
4. Use the arrows, Alt+Left/Right or a mouse's side buttons to go back and forward, and Ctrl+R or F5 to reload. Ctrl+F finds text on the page (Enter for the next match, Ctrl+Shift+G for the previous one, Esc to close), and Ctrl+plus, Ctrl+minus and Ctrl+0 change the zoom.
5. Downloads go to your Downloads folder, where Files shows them. Cookies and site data stay between visits.

## Expected result and troubleshooting
Voyage is WaveOS's own browser around WebKitGTK, Debian's build of WebKit, running on WaveOS's Linux layer: sites see a browser like Safari, with JavaScript (JavaScriptCore), CSS layout, web fonts, video and audio. It follows WaveOS's light or dark appearance when it starts, and pages that offer a dark style get it. It takes about fifteen seconds to show a page in a virtual machine that emulates the processor (as on Macs with Apple silicon), half the time Firefox takes, and much less where the virtual machine uses the processor's own virtualization. Its tabs share one web process, so new tabs open quickly.

Voyage comes with the Linux packages on the disk. If they aren't there, [Firefox](apps-firefox.md) opens instead when it is on the disk, or else [NetSurf](apps-netsurf.md). WebKit's sandbox is off on WaveOS, which doesn't have the Linux features it needs yet: browse sites you trust. Pages draw in software.

See [Desktop and windows](desktop-windows.md) for shared shortcuts and [Recovery](reference-recovery.md) if the app cannot open.
