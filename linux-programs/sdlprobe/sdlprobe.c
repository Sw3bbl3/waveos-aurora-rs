// sdlprobe: does SDL, unmodified, recognise WaveOS's game controllers? It
// runs Debian's libSDL2 (from the Linux packages on the disk), which finds
// controllers through libudev (or inotify with SDL_JOYSTICK_DISABLE_UDEV=1)
// and /dev/input, and reports each joystick: its name, GUID, whether SDL
// treats it as a game controller, and the mapping SDL chose. Without a
// mapping from its built-in database, SDL would not call it a game
// controller.
//
//   sdlprobe [seconds]     waits that long (default 3) for controllers to
//                          be plugged in, then reports; 2>/dev/kmsg puts
//                          the report in the system log.
//
// SDL's own headers aren't needed: the few declarations used are here.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    uint8_t data[16];
} SDL_JoystickGUID;
typedef struct SDL_GameController SDL_GameController;
typedef union {
    uint32_t type;
    uint8_t padding[56];
} SDL_Event;

#define SDL_INIT_GAMECONTROLLER 0x00002000u
#define SDL_CONTROLLERDEVICEADDED 0x653

extern int SDL_Init(uint32_t flags);
extern void SDL_Quit(void);
extern const char *SDL_GetError(void);
extern const char *SDL_GetRevision(void);
extern int SDL_NumJoysticks(void);
extern const char *SDL_JoystickNameForIndex(int index);
extern SDL_JoystickGUID SDL_JoystickGetDeviceGUID(int index);
extern void SDL_JoystickGetGUIDString(SDL_JoystickGUID guid, char *out, int len);
extern int SDL_IsGameController(int index);
extern char *SDL_GameControllerMappingForDeviceIndex(int index);
extern const char *SDL_GameControllerNameForIndex(int index);
extern SDL_GameController *SDL_GameControllerOpen(int index);
extern int SDL_GameControllerHasRumble(SDL_GameController *c);
extern void SDL_GameControllerClose(SDL_GameController *c);
extern int SDL_PollEvent(SDL_Event *e);
extern void SDL_Delay(uint32_t ms);
extern void SDL_free(void *p);

int main(int argc, char **argv) {
    int seconds = argc > 1 ? atoi(argv[1]) : 3;
    if (SDL_Init(SDL_INIT_GAMECONTROLLER) != 0) {
        fprintf(stderr, "sdlprobe: SDL_Init failed: %s\n", SDL_GetError());
        return 2;
    }
    fprintf(stderr, "sdlprobe: SDL %s\n", SDL_GetRevision());
    int added = 0;
    for (int t = 0; t < seconds * 10; t++) {
        SDL_Event e;
        while (SDL_PollEvent(&e))
            if (e.type == SDL_CONTROLLERDEVICEADDED) added++;
        SDL_Delay(100);
    }
    int n = SDL_NumJoysticks(), controllers = 0;
    fprintf(stderr, "sdlprobe: %d joystick(s), %d controller-added event(s)\n", n, added);
    for (int i = 0; i < n; i++) {
        char guid[64];
        SDL_JoystickGetGUIDString(SDL_JoystickGetDeviceGUID(i), guid, sizeof guid);
        int is = SDL_IsGameController(i);
        fprintf(stderr, "sdlprobe: joystick %d \"%s\" guid %s: %s\n", i, SDL_JoystickNameForIndex(i), guid,
                is ? "game controller" : "NOT a game controller");
        if (!is) continue;
        controllers++;
        char *mapping = SDL_GameControllerMappingForDeviceIndex(i);
        fprintf(stderr, "sdlprobe:   name \"%s\", mapping %s\n", SDL_GameControllerNameForIndex(i),
                mapping ? mapping : "(none)");
        SDL_free(mapping);
        SDL_GameController *c = SDL_GameControllerOpen(i);
        if (c) {
            fprintf(stderr, "sdlprobe:   opened; rumble %s\n", SDL_GameControllerHasRumble(c) ? "yes" : "no");
            SDL_GameControllerClose(c);
        }
    }
    fprintf(stderr, "sdlprobe: %s\n", controllers ? "PASS: SDL recognises the controller(s)" : "FAIL: no game controller");
    SDL_Quit();
    return controllers ? 0 : 1;
}
