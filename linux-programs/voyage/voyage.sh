#!/bin/sh
# Voyage, WaveOS's web browser (/System/Linux/bin/voyage): starts the
# Wayland bridge if it isn't running, then Voyage (WebKit) from the Linux
# packages on the disk. WebKit's sandbox needs Linux features WaveOS
# doesn't have yet (namespaces, seccomp), so it is off; pages draw in
# software. `--waveos-dark` (from the Voyage app, when WaveOS is dark) gives
# it the dark theme, which pages see as prefers-color-scheme: dark.
if [ ! -e /usr/lib/voyage/voyage ]; then
    echo "voyage: Voyage isn't on this disk (cargo xtask run --fresh-disk adds it)" >&2
    exit 127
fi
if [ "$1" = "--waveos-dark" ]; then
    export GTK_THEME=Adwaita:dark
    shift
fi
wl-bridge --background || exit 1
export GDK_BACKEND=wayland WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 WEBKIT_DISABLE_COMPOSITING_MODE=1
exec /usr/lib/voyage/voyage "$@"
