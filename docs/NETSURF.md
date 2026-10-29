# NetSurf on WaveOS

NetSurf 3.11 is WaveOS's small web browser; the main one is [Firefox](LINUX.md#firefox), on the disk with the Linux packages. NetSurf is a separate GPL-2.0 program in `/System/NetSurf`, built the same way as Doom: a static Linux (musl) program that runs on the [Linux compatibility layer](LINUX.md) and draws straight into a WaveOS window through the WaveOS display calls (`libs/linux/include/waveos.h`). It needs no Linux root, no Wayland bridge and no X server.

It starts in seconds and opens wherever Firefox would when the Linux packages aren't on the disk. It replaced Nebula, the browser written for WaveOS, whose libraries stay: Learn lays out the handbook with the Nebula engine, `fetch` and apps use its HTTP and TLS, and `js` runs Pulsar.

## What it does

- HTML 4 and much of CSS 2.1 and CSS 3, with tables, floats and positioning.
- HTTP and HTTPS (curl with mbedTLS), checked against Mozilla's root certificates from Debian's `ca-certificates`.
- PNG, JPEG, GIF and BMP pictures, the site icon, and cookies kept in `~/.netsurf/Cookies`. SVG support is basic: many SVG pictures, such as Wikipedia's logo, show as an empty or red box.
- DejaVu fonts, drawn by FreeType.
- Clipboard shared with other apps; typing in any keyboard layout (characters come through as Unicode). Ctrl+U clears the address bar.
- Back and forward from the toolbar, Alt+Left and Alt+Right, a mouse's side buttons and a keyboard's browser keys.

There is no JavaScript: pages that need scripts show what their HTML and CSS give. There are no tabs; open another window from Shoal or Seek.

## Using it

Open **NetSurf** from Shoal or Seek. Without Firefox on the disk, the dock's browser, `.html` files and web links in Learn and About open in it too. In Terminal, `/System/NetSurf/netsurf https://example.com` opens a page, and a path such as `/Documents/page.html` opens a local file.

Click in the address bar, type an address and press Enter. The centre of the window's top edge belongs to the window handle: click the address bar to either side of the centre.

## How it is built

`cargo xtask build` builds it (`xtask/src/netsurf.rs`). It downloads, each pinned by SHA-256:

| What | Version | Source |
|---|---|---|
| NetSurf, with its own libraries (libcss, libdom, hubbub, libnsfb, …) | 3.11 | netsurf-browser.org, full source release |
| curl | 8.14.1 | Debian 13: upstream source and Debian's patches |
| mbed TLS | 3.6.6 | Debian 13 |
| zlib | 1.3.1 | Debian 13 |
| libpng | 1.6.48 | Debian 13 |
| libjpeg-turbo | 2.1.5 | Debian 13 |
| FreeType | 2.13.3 | Debian 13 |
| expat | 2.8.3 | Debian 13 |
| ca-certificates, fonts-dejavu | 20250419, 2.37 | Debian 13 packages |

It compiles about a thousand C files with the same compiler as the other Linux programs (`musl-gcc`, or Clang with Debian's musl on macOS), in parallel, and keeps the objects for the next build.

NetSurf's own build generates some sources with Perl, gperf and small C programs. xtask writes those itself instead (character set aliases, HTML entities, element and attribute lookups, the CSS property parsers, the toolbar pictures and the messages file), so building needs nothing beyond the C compiler, `tar` and `patch`. The library configuration headers that `configure` or CMake would write are in `third_party/netsurf/config`.

WaveOS's own parts:

- `third_party/netsurf/waveos/surface.c`: a libnsfb surface that is a WaveOS window. It turns WaveOS keyboard, pointer, scroll, resize and close events into libnsfb events and presents what NetSurf draws. At 2× display scale it draws at full resolution; the front end then measures text at the size it is drawn and makes the toolbar buttons twice as wide.
- `third_party/netsurf/waveos/netsurf.c`: defaults (window size, scale, fonts, certificates, cookies), local paths as `file:` addresses, and the WaveOS clipboard.
- `third_party/netsurf/patches`: small changes to NetSurf's framebuffer front end that call into the two files above, and UTF-8 text in its text fields.

Set `WAVEOS_NSFB_DEBUG=1` to have the surface log each event it passes on to standard error.

### Networks that inspect HTTPS

Some company and school networks replace sites' certificates with their own, signed by a root certificate of their own. To trust it, build with `WAVEOS_EXTRA_CA` set to a PEM file holding that root:

```sh
WAVEOS_EXTRA_CA=/path/to/company-root.pem cargo xtask build
```

The certificates in the file are added to `/System/NetSurf/ca-bundle.pem`.

## Licenses

NetSurf is GPL-2.0; its own libraries are MIT. curl, mbed TLS (Apache-2.0), zlib, libpng, libjpeg-turbo (IJG and BSD), FreeType (FTL) and expat (MIT) carry their own licenses. The DejaVu fonts carry the Bitstream Vera license, and the root certificates come from Mozilla (MPL-2.0). Every license file goes into `/System/Licenses` (`netsurf.txt` and `netsurf-*.txt`), and [THIRD_PARTY_NOTICES](../THIRD_PARTY_NOTICES.md) lists them.
