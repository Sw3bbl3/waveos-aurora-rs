# doomgeneric

The Doom engine WaveOS runs as a Linux program (`/System/Games/Doom/doom`).

- Upstream: <https://github.com/ozkl/doomgeneric>, commit
  `e257a09527062bfb986b6e5e0780839b6cbd5b71`. It derives from Chocolate Doom
  and id Software's Doom source release.
- License: GNU GPL version 2 or later ([COPYING](COPYING)). WaveOS ships it
  as a separate program, with this license in `/System/Licenses/doomgeneric.txt`.
- [`src/`](src) is upstream's `doomgeneric/` folder, unchanged, without the
  other platforms' backends (Allegro, SDL, X11, Windows, Emscripten, the Linux
  console and soso) and the Visual Studio files.
- [`waveos/`](waveos) is WaveOS's part: the backend (`doomgeneric_waveos.c`:
  the window, keys, clock and a sound-effect mixer) and an empty `SDL_mixer.h`
  that `i_sound.c` includes when sound is on.

`cargo xtask build` compiles it with `musl-gcc -static` and the WaveOS calls
in `libs/linux/include/waveos.h`. See [docs/LINUX.md](../../docs/LINUX.md).
