# Nebula
Read simple websites and local HTML documents.

## Use the app
1. Open Nebula and press Ctrl+L to enter an address or search.
2. Use Alt+Left/Right for history and Ctrl+R to reload.
3. Use Ctrl+F to find text and Ctrl+plus/minus/0 to adjust zoom.
4. Open downloaded files through Files; inspect the destination shown by the browser.
5. Use Ctrl+T or the + button for a new tab (up to eight), Ctrl+Tab / Ctrl+Shift+Tab to switch, and Ctrl+1–8 to select a tab. Ctrl+9 selects the last tab.
6. Ctrl+W closes the active tab; Ctrl+Shift+T reopens a recently closed address. Reopening reloads the page. Ctrl-click a link to open it in a new tab.
7. Use the Home button or Alt+Home for the start page. Try the offline Layout gallery and Pulsar playground from there.

Background tabs throttle script timers to about once per second and release their layout caches. Storage updates can reach other tabs through storage events. Their document, scroll position, zoom and typed form values remain available when you return. Each tab keeps up to 100 history entries and 12 MiB of decoded image pixels; very large images are reduced. Closing the window or Alt+F4 closes all its tabs. Tabs are not restored after quitting.

## Expected result and troubleshooting
Nebula supports HTML/CSS, flexbox, grid, bounded nested stylesheet imports, responsive media attributes, common raster images, HTTP and TLS. Pulsar provides JavaScript, DOM events, timers, storage and network requests; cookies are supported. It is a custom engine with partial web compatibility: complex sites can remain incomplete, and JavaScript modules, WebP and SVG are not supported. TLS errors are not bypassed.

See [Desktop and windows](desktop-windows.md) for shared shortcuts and [Recovery](reference-recovery.md) if the app cannot open.

## Native video and audio
1. Open a local or reachable MP4 URL containing supported H.264 video and AAC-LC audio.
2. Use the native play/pause, seek, mute, and volume controls. Save other work before trying a large clip.
3. If decoding fails, check the format and the displayed error. Try the documented small test fixture before a complex streaming site.

The player decodes on worker threads and synchronizes presentation with the audio output. The bounded decoder accepts video up to 1920×1088 and mono/stereo AAC-LC up to 48 kHz. Fragmented MP4 and part of Media Source Extensions are implemented; this does not establish DRM or general streaming-site compatibility. Unsupported codecs and damaged files may be rejected. See [Pulsar and media development](developer-pulsar.md).
