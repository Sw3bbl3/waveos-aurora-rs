/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: memory allocation. kmalloc and friends come from the
 * WaveOS kernel heap (always physically mapped and zeroable); GFP flags only
 * matter for __GFP_ZERO.
 */
#ifndef _WAVEOS_SLAB_H
#define _WAVEOS_SLAB_H

#include <waveos/base.h>

#define ___GFP_ZERO 0x100u
#define __GFP_ZERO ((gfp_t)___GFP_ZERO)
#define __GFP_NOWARN ((gfp_t)0x200u)
#define __GFP_RETRY_MAYFAIL ((gfp_t)0x400u)
#define __GFP_NORETRY ((gfp_t)0x800u)
#define __GFP_NOFAIL ((gfp_t)0x1000u)
#define __GFP_DMA32 ((gfp_t)0x2000u)
#define __GFP_DMA ((gfp_t)0x4000u)
#define __GFP_HIGHMEM ((gfp_t)0x8000u)
#define __GFP_COMP ((gfp_t)0x10000u)
#define __GFP_RECLAIM ((gfp_t)0x20000u)
#define __GFP_ACCOUNT ((gfp_t)0x40000u)
#define __GFP_NOMEMALLOC ((gfp_t)0x80000u)
#define __GFP_MOVABLE ((gfp_t)0x100000u)
#define __GFP_RECLAIMABLE ((gfp_t)0x200000u)
#define GFP_KERNEL ((gfp_t)0x1u)
#define GFP_ATOMIC ((gfp_t)0x2u)
#define GFP_NOWAIT ((gfp_t)0x4u)
#define GFP_NOIO ((gfp_t)0x8u)
#define GFP_NOFS ((gfp_t)0x10u)
#define GFP_USER ((gfp_t)0x20u)
#define GFP_DMA GFP_KERNEL
#define GFP_DMA32 GFP_KERNEL
#define GFP_HIGHUSER GFP_USER
#define GFP_HIGHUSER_MOVABLE GFP_USER
#define GFP_KERNEL_ACCOUNT GFP_KERNEL
#define GFP_TRANSHUGE GFP_KERNEL
#define ZERO_SIZE_PTR ((void *)16)
#define ZERO_OR_NULL_PTR(x) ((unsigned long)(x) <= (unsigned long)ZERO_SIZE_PTR)
#define ARCH_KMALLOC_MINALIGN 16
#define KMALLOC_MAX_SIZE (1UL << 30)

void *__kmalloc(size_t size, gfp_t flags) __malloc;
void kfree(const void *p);
void *krealloc(const void *p, size_t size, gfp_t flags);
size_t ksize(const void *p);
void kfree_sensitive(const void *p);
#define kfree_const(p) kfree(p)
static inline void *kmalloc(size_t size, gfp_t flags) { return __kmalloc(size, flags); }
static inline void *kzalloc(size_t size, gfp_t flags) { return __kmalloc(size, flags | __GFP_ZERO); }
static inline void *kmalloc_array(size_t n, size_t size, gfp_t flags)
{
    if (size != 0 && n > SIZE_MAX / size) return NULL;
    return __kmalloc(n * size, flags);
}
static inline void *kcalloc(size_t n, size_t size, gfp_t flags) { return kmalloc_array(n, size, flags | __GFP_ZERO); }
static inline void *krealloc_array(void *p, size_t n, size_t size, gfp_t flags)
{
    if (size != 0 && n > SIZE_MAX / size) return NULL;
    return krealloc(p, n * size, flags);
}
#define kmalloc_node(size, flags, node) kmalloc(size, flags)
#define kzalloc_node(size, flags, node) kzalloc(size, flags)
#define kvmalloc(size, flags) kmalloc(size, flags)
#define kvzalloc(size, flags) kzalloc(size, flags)
#define kvmalloc_array(n, size, flags) kmalloc_array(n, size, flags)
#define kvcalloc(n, size, flags) kcalloc(n, size, flags)
#define kvrealloc(p, size, flags) krealloc(p, size, flags)
#define kvfree(p) kfree(p)
#define kvfree_sensitive(p, len) kfree(p)
#define vmalloc(size) kmalloc(size, GFP_KERNEL)
#define vzalloc(size) kzalloc(size, GFP_KERNEL)
#define vfree(p) kfree(p)
#define kfree_rcu(p, field) kfree(p)
#define kvfree_rcu(p, ...) kvfree(p)
static inline void *kmemdup(const void *src, size_t len, gfp_t gfp)
{
    void *p = kmalloc(len, gfp);
    if (p) __builtin_memcpy(p, src, len);
    return p;
}
#define kvmemdup kmemdup
char *kstrdup(const char *s, gfp_t gfp);
const char *kstrdup_const(const char *s, gfp_t gfp);
char *kstrndup(const char *s, size_t max, gfp_t gfp);
char *kmemdup_nul(const char *s, size_t len, gfp_t gfp);
void *memdup_user(const void __user *src, size_t len);
void *vmemdup_user(const void __user *src, size_t len);

/* Slab caches: plain kmalloc of the object size. */
struct kmem_cache {
    size_t size;
    void (*ctor)(void *);
    const char *name;
};
struct kmem_cache *kmem_cache_create(const char *name, unsigned int size, unsigned int align, slab_flags_t flags,
                                     void (*ctor)(void *));
void kmem_cache_destroy(struct kmem_cache *s);
void *kmem_cache_alloc(struct kmem_cache *s, gfp_t flags);
static inline void *kmem_cache_zalloc(struct kmem_cache *s, gfp_t flags) { return kmem_cache_alloc(s, flags | __GFP_ZERO); }
void kmem_cache_free(struct kmem_cache *s, void *p);
#define KMEM_CACHE(s, flags) kmem_cache_create(#s, sizeof(struct s), __alignof__(struct s), (flags), NULL)
#define SLAB_HWCACHE_ALIGN 0x2000u
#define SLAB_RECLAIM_ACCOUNT 0x20000u
#define SLAB_TYPESAFE_BY_RCU 0x80000u
#define SLAB_ACCOUNT 0x4000000u

#endif
