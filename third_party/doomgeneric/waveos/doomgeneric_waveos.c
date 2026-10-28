// doomgeneric for WaveOS: the game runs as a Linux program and draws in a
// WaveOS window through the calls in waveos.h. Sound effects are mixed here
// and played on a WaveOS sound stream; music is not played.
//
// Copyright (C) 2026 The WaveOS authors.
//
// This program is free software; you can redistribute it and/or
// modify it under the terms of the GNU General Public License
// as published by the Free Software Foundation; either version 2
// of the License, or (at your option) any later version.

#include <ctype.h>
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include <waveos.h>

#include "doomgeneric.h"
#include "doomkeys.h"
#include "i_sound.h"
#include "w_wad.h"
#include "z_zone.h"

// ---- Window ----------------------------------------------------------------

static long window = -1;
static struct wave_surface surface;

void DG_Init(void)
{
    static const char title[] = "Doom";
    window = wave_win_create(DOOMGENERIC_RESX, DOOMGENERIC_RESY, title, sizeof title - 1, WAVE_WIN_RESIZABLE);
    if (window < 0 || wave_win_surface(window, &surface) < 0)
    {
        fprintf(stderr, "doom: no window (error %ld)\n", -window);
        exit(1);
    }
}

// Scales the frame to fill the window, keeping its shape, with black bars.
void DG_DrawFrame(void)
{
    uint32_t *dst = (uint32_t *)(uintptr_t)surface.addr;
    uint32_t w = surface.width, h = surface.height, stride = surface.stride;
    if (!dst || !w || !h)
        return;
    uint32_t fw = w, fh = (uint32_t)((uint64_t)w * DOOMGENERIC_RESY / DOOMGENERIC_RESX);
    if (fh > h)
    {
        fh = h;
        fw = (uint32_t)((uint64_t)h * DOOMGENERIC_RESX / DOOMGENERIC_RESY);
    }
    uint32_t ox = (w - fw) / 2, oy = (h - fh) / 2;
    for (uint32_t y = 0; y < h; y++)
    {
        uint32_t *row = dst + (size_t)y * stride;
        if (y < oy || y >= oy + fh)
        {
            for (uint32_t x = 0; x < w; x++)
                row[x] = 0xFF000000;
            continue;
        }
        const pixel_t *src = DG_ScreenBuffer + (size_t)((y - oy) * DOOMGENERIC_RESY / fh) * DOOMGENERIC_RESX;
        for (uint32_t x = 0; x < ox; x++)
            row[x] = 0xFF000000;
        uint32_t step = (DOOMGENERIC_RESX << 16) / fw, sx = 0;
        for (uint32_t x = 0; x < fw; x++, sx += step)
            row[ox + x] = 0xFF000000 | src[sx >> 16];
        for (uint32_t x = ox + fw; x < w; x++)
            row[x] = 0xFF000000;
    }
    wave_win_present(window, 0, 0, w, h);
}

void DG_SetWindowTitle(const char *title)
{
    if (window >= 0)
        wave_win_set_title(window, title, strlen(title));
}

void DG_SleepMs(uint32_t ms)
{
    struct timespec t = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&t, NULL);
}

uint32_t DG_GetTicksMs(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint32_t)(t.tv_sec * 1000 + t.tv_nsec / 1000000);
}

// ---- Keys ------------------------------------------------------------------

static unsigned char doom_key(const struct wave_event *e)
{
    switch (e->a)
    {
    case WAVE_KEY_ESCAPE: return KEY_ESCAPE;
    case WAVE_KEY_ENTER: return KEY_ENTER;
    case WAVE_KEY_BACKSPACE: return KEY_BACKSPACE;
    case WAVE_KEY_TAB: return KEY_TAB;
    case WAVE_KEY_LEFT: return KEY_LEFTARROW;
    case WAVE_KEY_RIGHT: return KEY_RIGHTARROW;
    case WAVE_KEY_UP: return KEY_UPARROW;
    case WAVE_KEY_DOWN: return KEY_DOWNARROW;
    case WAVE_KEY_HOME: return KEY_HOME;
    case WAVE_KEY_END: return KEY_END;
    case WAVE_KEY_PAGE_UP: return KEY_PGUP;
    case WAVE_KEY_PAGE_DOWN: return KEY_PGDN;
    case WAVE_KEY_INSERT: return KEY_INS;
    case WAVE_KEY_DELETE: return KEY_DEL;
    case WAVE_KEY_SHIFT: return KEY_RSHIFT;
    case WAVE_KEY_CTRL: return KEY_FIRE;
    case WAVE_KEY_ALT: return KEY_LALT;
    case WAVE_KEY_CAPS_LOCK: return KEY_CAPSLOCK;
    case WAVE_KEY_CHAR:
        if (e->b == ' ')
            return KEY_USE;
        if (e->b == '+')
            return KEY_EQUALS;
        if (e->b == '_')
            return KEY_MINUS;
        return e->b < 0x80 ? (unsigned char)tolower((int)e->b) : 0;
    default:
        if (e->a >= WAVE_KEY_F1 && e->a <= WAVE_KEY_F12)
        {
            static const unsigned char f[12] = {KEY_F1, KEY_F2, KEY_F3, KEY_F4, KEY_F5, KEY_F6,
                                                KEY_F7, KEY_F8, KEY_F9, KEY_F10, KEY_F11, KEY_F12};
            return f[e->a - WAVE_KEY_F1];
        }
        return 0;
    }
}

int DG_GetKey(int *pressed, unsigned char *key)
{
    struct wave_event e;
    while (wave_next_event(&e, 0) == 1)
    {
        switch (e.kind)
        {
        case WAVE_EV_CLOSE_REQUESTED:
            exit(0);
        case WAVE_EV_RESIZE:
            wave_win_surface(window, &surface);
            break;
        case WAVE_EV_KEY:
        {
            unsigned char k = doom_key(&e);
            if (k)
            {
                *pressed = e.c != 0;
                *key = k;
                return 1;
            }
            break;
        }
        }
    }
    return 0;
}

// ---- Sound effects ---------------------------------------------------------

// Not used: there is no libsamplerate here, but the settings file names them.
int use_libsamplerate = 0;
float libsamplerate_scale = 0.65f;

#define CHANNELS 16
#define CHUNK 512 // frames per write: about 11 ms

struct channel
{
    const uint8_t *data; // unsigned 8-bit samples
    uint32_t length;     // in samples
    uint32_t pos;        // 16.16 fixed point
    uint32_t step;       // source samples per output frame, 16.16
    int left, right;     // 0..254
};

static struct channel channels[CHANNELS];
static pthread_mutex_t mixer = PTHREAD_MUTEX_INITIALIZER;
static int audio_fd = -1;
static boolean sfx_prefix;

static void *mix_loop(void *arg)
{
    (void)arg;
    static int16_t out[CHUNK * 2];
    for (;;)
    {
        pthread_mutex_lock(&mixer);
        for (int i = 0; i < CHUNK; i++)
        {
            int l = 0, r = 0;
            for (int c = 0; c < CHANNELS; c++)
            {
                struct channel *ch = &channels[c];
                if (!ch->data)
                    continue;
                // 8-bit samples to 16-bit; at the default effects volume
                // a full-scale sound peaks near a quarter of full scale.
                int s = ((int)ch->data[ch->pos >> 16] - 128) * 256;
                l += s * ch->left / 254;
                r += s * ch->right / 254;
                ch->pos += ch->step;
                if ((ch->pos >> 16) >= ch->length)
                    ch->data = NULL;
            }
            out[i * 2] = l > 32767 ? 32767 : l < -32768 ? -32768 : l;
            out[i * 2 + 1] = r > 32767 ? 32767 : r < -32768 ? -32768 : r;
        }
        pthread_mutex_unlock(&mixer);
        // Blocks while the stream's buffer is full, which paces the loop.
        if (write(audio_fd, out, sizeof out) < 0 && errno != EINTR)
            return NULL;
    }
}

static boolean Snd_Init(boolean use_sfx_prefix)
{
    sfx_prefix = use_sfx_prefix;
    audio_fd = (int)wave_audio_open();
    if (audio_fd < 0)
        return false;
    pthread_t t;
    if (pthread_create(&t, NULL, mix_loop, NULL) != 0)
    {
        close(audio_fd);
        return false;
    }
    pthread_detach(t);
    return true;
}

static void Snd_Shutdown(void)
{
    pthread_mutex_lock(&mixer);
    memset(channels, 0, sizeof channels);
    pthread_mutex_unlock(&mixer);
}

static int Snd_GetSfxLumpNum(sfxinfo_t *sfx)
{
    char name[16];
    if (sfx->link)
        sfx = sfx->link;
    snprintf(name, sizeof name, sfx_prefix ? "ds%s" : "%s", sfx->name);
    return W_GetNumForName(name);
}

static void Snd_Update(void)
{
}

static void set_params(struct channel *ch, int vol, int sep)
{
    if (sep < 0)
        sep = 0;
    if (sep > 254)
        sep = 254;
    ch->left = (254 - sep) * vol / 127;
    ch->right = sep * vol / 127;
}

static void Snd_UpdateSoundParams(int channel, int vol, int sep)
{
    if (channel < 0 || channel >= CHANNELS)
        return;
    pthread_mutex_lock(&mixer);
    set_params(&channels[channel], vol, sep);
    pthread_mutex_unlock(&mixer);
}

// A sound lump: format 3, the rate, the length, then the samples between
// 16 bytes of padding on each side.
static int Snd_StartSound(sfxinfo_t *sfx, int channel, int vol, int sep)
{
    if (channel < 0 || channel >= CHANNELS)
        return -1;
    int lump = sfx->lumpnum;
    const uint8_t *data = W_CacheLumpNum(lump, PU_STATIC);
    int lumplen = W_LumpLength(lump);
    if (lumplen < 8 || data[0] != 3 || data[1] != 0)
        return -1;
    uint32_t rate = data[2] | data[3] << 8;
    uint32_t length = data[4] | data[5] << 8 | data[6] << 16 | (uint32_t)data[7] << 24;
    if (length > (uint32_t)lumplen - 8 || length <= 48 || rate == 0)
        return -1;
    pthread_mutex_lock(&mixer);
    struct channel *ch = &channels[channel];
    ch->data = data + 8 + 16;
    ch->length = length - 32;
    ch->pos = 0;
    ch->step = (uint32_t)(((uint64_t)rate << 16) / WAVE_AUDIO_RATE);
    set_params(ch, vol, sep);
    pthread_mutex_unlock(&mixer);
    return channel;
}

static void Snd_StopSound(int channel)
{
    if (channel < 0 || channel >= CHANNELS)
        return;
    pthread_mutex_lock(&mixer);
    channels[channel].data = NULL;
    pthread_mutex_unlock(&mixer);
}

static boolean Snd_SoundIsPlaying(int channel)
{
    if (channel < 0 || channel >= CHANNELS)
        return false;
    pthread_mutex_lock(&mixer);
    boolean playing = channels[channel].data != NULL;
    pthread_mutex_unlock(&mixer);
    return playing;
}

static void Snd_CacheSounds(sfxinfo_t *sounds, int num_sounds)
{
    (void)sounds;
    (void)num_sounds;
}

static snddevice_t sound_devices[] = {SNDDEVICE_SB, SNDDEVICE_PAS, SNDDEVICE_GUS,
                                      SNDDEVICE_WAVEBLASTER, SNDDEVICE_SOUNDCANVAS, SNDDEVICE_AWE32};

sound_module_t DG_sound_module = {
    sound_devices,
    sizeof sound_devices / sizeof sound_devices[0],
    Snd_Init,
    Snd_Shutdown,
    Snd_GetSfxLumpNum,
    Snd_Update,
    Snd_UpdateSoundParams,
    Snd_StartSound,
    Snd_StopSound,
    Snd_SoundIsPlaying,
    Snd_CacheSounds,
};

// ---- Music (silent) --------------------------------------------------------

static boolean Mus_Init(void) { return true; }
static void Mus_Nothing(void) {}
static void Mus_SetVolume(int volume) { (void)volume; }
static void *Mus_Register(void *data, int len) { (void)data; (void)len; return (void *)1; }
static void Mus_Unregister(void *handle) { (void)handle; }
static void Mus_Play(void *handle, boolean looping) { (void)handle; (void)looping; }
static boolean Mus_IsPlaying(void) { return false; }

music_module_t DG_music_module = {
    NULL, 0, Mus_Init, Mus_Nothing, Mus_SetVolume, Mus_Nothing, Mus_Nothing,
    Mus_Register, Mus_Unregister, Mus_Play, Mus_Nothing, Mus_IsPlaying, Mus_Nothing,
};

// ---- Start -----------------------------------------------------------------

int main(int argc, char **argv)
{
    // Settings and saved games go in ~/.doom.
    const char *home = getenv("HOME");
    if (home)
    {
        char dir[512];
        snprintf(dir, sizeof dir, "%s/.doom", home);
        mkdir(home, 0755);
        mkdir(dir, 0755);
        if (chdir(dir) != 0)
            fprintf(stderr, "doom: can't use %s: %s\n", dir, strerror(errno));
    }
    doomgeneric_Create(argc, argv);
    for (;;)
        doomgeneric_Tick();
}
