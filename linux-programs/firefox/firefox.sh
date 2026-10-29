#!/bin/sh
# Firefox ESR on WaveOS (/System/Linux/bin/firefox): starts the Wayland
# bridge if it isn't running, then Debian's Firefox ESR from the Linux
# packages on the disk (cargo xtask run puts them there). Firefox's
# sandboxes need Linux features WaveOS doesn't have yet (seccomp,
# namespaces), so they are off. `--waveos-dark` (from the Firefox app, when
# WaveOS is dark) gives Firefox the dark GTK theme.
if [ ! -e /usr/lib/firefox-esr/firefox-esr ]; then
    echo "firefox: Firefox ESR isn't on this disk (cargo xtask run --fresh-disk adds it)" >&2
    exit 127
fi
if [ "$1" = "--waveos-dark" ]; then
    export GTK_THEME=Adwaita:dark
    shift
fi
wl-bridge --background || exit 1
export MOZ_ENABLE_WAYLAND=1 MOZ_CRASHREPORTER_DISABLE=1 \
    MOZ_DISABLE_CONTENT_SANDBOX=1 MOZ_DISABLE_RDD_SANDBOX=1 MOZ_DISABLE_GMP_SANDBOX=1 \
    MOZ_DISABLE_SOCKET_PROCESS_SANDBOX=1 MOZ_DISABLE_UTILITY_SANDBOX=1 MOZ_DISABLE_GPU_SANDBOX=1
exec firefox-esr "$@"
