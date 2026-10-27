/* SPDX-License-Identifier: MIT */
/* WaveOS LinuxKPI: the core kernel API in one include (most linux/*.h names lead here). */
#ifndef _WAVEOS_KERNEL_H
#define _WAVEOS_KERNEL_H

#include <waveos/base.h>
#include <waveos/errno.h>
#include <waveos/printk.h>
#include <waveos/string.h>
#include <waveos/slab.h>
#include <waveos/list.h>
#include <waveos/atomic.h>
#include <waveos/sync.h>
#include <waveos/time.h>
#include <waveos/ioctl.h>

void sort(void *base, size_t num, size_t size, int (*cmp)(const void *, const void *),
          void (*swap)(void *, void *, int));
void sort_r(void *base, size_t num, size_t size, int (*cmp)(const void *, const void *, const void *),
            void (*swap)(void *, void *, int), const void *priv);
static inline unsigned long *bitmap_zalloc(unsigned int nbits, gfp_t flags)
{ return kcalloc(BITS_TO_LONGS(nbits), sizeof(unsigned long), flags); }
static inline void bitmap_free(const unsigned long *b) { kfree(b); }

#endif
