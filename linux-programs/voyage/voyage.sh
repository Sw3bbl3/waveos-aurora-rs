#!/bin/sh
# Voyage, WaveOS's web browser (/System/Linux/bin/voyage): starts the
# Wayland bridge if it isn't running, then Voyage (WebKit) from the Linux
# packages on the disk. WebKit's sandbox needs Linux features WaveOS
# doesn't have yet (namespaces, seccomp), so it is off; pages draw in
# software, composited as WebKit expects (with Mesa's llvmpipe), and sound goes
# to WaveOS through OSS (/dev/dsp). `--waveos-dark` (from the Voyage app, when WaveOS is dark) gives
# it the dark theme, which pages see as prefers-color-scheme: dark.
if [ ! -e /usr/lib/voyage/voyage ]; then
    echo "voyage: Voyage isn't on this disk (cargo xtask run --fresh-disk adds it)" >&2
    exit 127
fi
if [ "$1" = "--waveos-dark" ]; then
    export GTK_THEME=Adwaita:dark
    shift
fi
# WebKit's network process stops without GLib's compiled settings schemas.
# When the disk's are missing or older than its packages (the build machine
# had no glib-compile-schemas), Voyage makes its own copy once.
schemas=/usr/share/glib-2.0/schemas
if ! grep -q org.gnome.system.proxy "$schemas/gschemas.compiled" 2>/dev/null; then
    own="$HOME/.cache/voyage/schemas"
    if ! grep -q org.gnome.system.proxy "$own/gschemas.compiled" 2>/dev/null; then
        mkdir -p "$own" && /usr/lib/x86_64-linux-gnu/glib-2.0/glib-compile-schemas --targetdir="$own" "$schemas"
    fi
    export GSETTINGS_SCHEMA_DIR="$own"
fi
wl-bridge --background || exit 1
export GDK_BACKEND=wayland WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
# Skia draws pages on the processor: its GL path, on Mesa's software
# renderer, loses glyphs (whole tiles of text show as blocks).
export WEBKIT_SKIA_ENABLE_CPU_RENDERING=1
# Straight to the OSS sink: PulseAudio and JACK aren't there, and the media
# clock only runs once an audio sink plays.
export GST_PLUGIN_FEATURE_RANK=osssink:MAX,pulsesink:NONE,jackaudiosink:NONE,oss4sink:NONE
exec /usr/lib/voyage/voyage "$@"
