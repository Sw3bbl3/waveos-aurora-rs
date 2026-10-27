// SPDX-License-Identifier: MIT
/*
 * WaveOS LinuxKPI: kmalloc, slab caches, pages, vmap, scatterlists and
 * coherent DMA memory.
 *
 * kmalloc carves from the WaveOS kernel heap, which is physically contiguous
 * and lies in the physical window, so heap buffers have physical addresses.
 * Each allocation carries a 16-byte header holding its size.
 *
 * A struct page exists only for frames somebody asked about: alloc_pages
 * makes them (an array, so `page + i` works within an allocation), and
 * waveos_page_for_phys makes one on demand for any other frame (a kmalloc
 * buffer handed to a scatterlist, say). A hash table finds them by address.
 */
#include <waveos/kernel.h>
#include <waveos/mm.h>

void *waveos_heap_alloc(size_t size);
void waveos_heap_free(void *p, size_t size);

#define HEADER 16

void *__kmalloc(size_t size, gfp_t flags)
{
    u64 *h;
    if (size > (1UL << 30))
        return NULL;
    h = waveos_heap_alloc(size + HEADER);
    if (!h)
        return NULL;
    h[0] = size;
    if (flags & __GFP_ZERO)
        memset((char *)h + HEADER, 0, size);
    return (char *)h + HEADER;
}

static inline u64 *header(const void *p) { return (u64 *)((char *)p - HEADER); }

size_t ksize(const void *p) { return p ? header(p)[0] : 0; }

void kfree(const void *p)
{
    if (!p || IS_ERR(p))
        return;
    waveos_heap_free(header(p), header(p)[0] + HEADER);
}

void kfree_sensitive(const void *p)
{
    if (p && !IS_ERR(p))
        memset((void *)p, 0, ksize(p));
    kfree(p);
}

void *krealloc(const void *p, size_t size, gfp_t flags)
{
    void *n;
    size_t old;
    if (!p)
        return __kmalloc(size, flags);
    if (!size) {
        kfree(p);
        return ZERO_SIZE_PTR;
    }
    old = ksize(p);
    if (size <= old)
        return (void *)p;
    n = __kmalloc(size, flags & ~__GFP_ZERO);
    if (!n)
        return NULL;
    memcpy(n, p, old);
    if (flags & __GFP_ZERO)
        memset((char *)n + old, 0, size - old);
    kfree(p);
    return n;
}

struct kmem_cache *kmem_cache_create(const char *name, unsigned int size, unsigned int align, slab_flags_t flags,
                                     void (*ctor)(void *))
{
    struct kmem_cache *s = kzalloc(sizeof(*s), GFP_KERNEL);
    (void)flags;
    if (!s)
        return NULL;
    /* The heap aligns to 16 bytes, past the header. */
    if (align > HEADER)
        size = ALIGN(size, align);
    s->size = size;
    s->ctor = ctor;
    s->name = name;
    return s;
}

void kmem_cache_destroy(struct kmem_cache *s) { kfree(s); }

void *kmem_cache_alloc(struct kmem_cache *s, gfp_t flags)
{
    void *p = __kmalloc(s->size, flags);
    if (p && s->ctor)
        s->ctor(p);
    return p;
}

void kmem_cache_free(struct kmem_cache *s, void *p)
{
    (void)s;
    kfree(p);
}

/* ---- struct page ---- */
#define BUCKETS 4096
static struct page *buckets[BUCKETS];
static DEFINE_SPINLOCK(pages_lock);

static inline unsigned int bucket(u64 phys) { return (phys >> PAGE_SHIFT) * 2654435761u % BUCKETS; }

static struct page *lookup(u64 phys)
{
    struct page *p;
    for (p = buckets[bucket(phys)]; p; p = p->waveos_next)
        if (p->phys == phys)
            return p;
    return NULL;
}

static void insert(struct page *p)
{
    unsigned int b = bucket(p->phys);
    p->waveos_next = buckets[b];
    buckets[b] = p;
}

static void remove(struct page *p)
{
    struct page **link = &buckets[bucket(p->phys)];
    while (*link && *link != p)
        link = &(*link)->waveos_next;
    if (*link)
        *link = p->waveos_next;
}

struct page *waveos_page_for_phys(u64 phys)
{
    unsigned long flags;
    struct page *p, *fresh;
    phys &= PAGE_MASK;
    spin_lock_irqsave(&pages_lock, flags);
    p = lookup(phys);
    spin_unlock_irqrestore(&pages_lock, flags);
    if (p)
        return p;
    fresh = kzalloc(sizeof(*fresh), GFP_ATOMIC);
    if (!fresh)
        return NULL;
    fresh->phys = phys;
    atomic_set(&fresh->refcount, 1);
    spin_lock_irqsave(&pages_lock, flags);
    p = lookup(phys);
    if (!p) {
        insert(fresh);
        p = fresh;
        fresh = NULL;
    }
    spin_unlock_irqrestore(&pages_lock, flags);
    kfree(fresh);
    return p;
}

struct page *alloc_pages(gfp_t gfp, unsigned int order)
{
    unsigned long n = 1UL << order, i, flags;
    struct page *pages;
    u64 phys = order ? waveos_frames_alloc_contiguous(n) : waveos_frame_alloc();
    if (!phys)
        return NULL;
    pages = kcalloc(n, sizeof(*pages), GFP_KERNEL);
    if (!pages) {
        for (i = 0; i < n; i++)
            waveos_frame_free(phys + i * PAGE_SIZE);
        return NULL;
    }
    if (gfp & __GFP_ZERO)
        memset(waveos_phys_to_virt(phys), 0, n * PAGE_SIZE);
    spin_lock_irqsave(&pages_lock, flags);
    for (i = 0; i < n; i++) {
        pages[i].phys = phys + i * PAGE_SIZE;
        atomic_set(&pages[i].refcount, 1);
        pages[i].waveos_order = order + 1;
        insert(&pages[i]);
    }
    spin_unlock_irqrestore(&pages_lock, flags);
    return pages;
}

void __free_pages(struct page *page, unsigned int order)
{
    unsigned long n = 1UL << order, i, flags;
    if (!page)
        return;
    if (page->waveos_order != order + 1) {
        WARN_ON(1); /* not the head of an alloc_pages allocation */
        return;
    }
    spin_lock_irqsave(&pages_lock, flags);
    for (i = 0; i < n; i++)
        remove(&page[i]);
    spin_unlock_irqrestore(&pages_lock, flags);
    for (i = 0; i < n; i++)
        waveos_frame_free(page[i].phys);
    kfree(page);
}

void put_page(struct page *p)
{
    if (atomic_dec_and_test(&p->refcount) && p->waveos_order)
        __free_pages(p, p->waveos_order - 1);
}

/* ---- vmap ---- */
struct vmap_area {
    struct vmap_area *next;
    void *addr;
    unsigned long count;
};
static struct vmap_area *vmaps;
static DEFINE_SPINLOCK(vmaps_lock);

void *vmap(struct page **pages, unsigned int count, unsigned long flags, pgprot_t prot)
{
    struct vmap_area *a;
    u64 *frames;
    unsigned long irq;
    (void)flags;
    (void)prot;
    if (!count)
        return NULL;
    frames = kmalloc_array(count, sizeof(u64), GFP_KERNEL);
    a = kmalloc(sizeof(*a), GFP_KERNEL);
    if (!frames || !a) {
        kfree(frames);
        kfree(a);
        return NULL;
    }
    for (unsigned int i = 0; i < count; i++)
        frames[i] = pages[i]->phys;
    a->addr = waveos_kmap_frames(frames, count);
    kfree(frames);
    if (!a->addr) {
        kfree(a);
        return NULL;
    }
    a->count = count;
    spin_lock_irqsave(&vmaps_lock, irq);
    a->next = vmaps;
    vmaps = a;
    spin_unlock_irqrestore(&vmaps_lock, irq);
    return a->addr;
}

void vunmap(const void *addr)
{
    struct vmap_area **link, *a = NULL;
    unsigned long irq;
    spin_lock_irqsave(&vmaps_lock, irq);
    for (link = &vmaps; *link; link = &(*link)->next) {
        if ((*link)->addr == addr) {
            a = *link;
            *link = a->next;
            break;
        }
    }
    spin_unlock_irqrestore(&vmaps_lock, irq);
    if (a) {
        waveos_kunmap(a->addr, a->count);
        kfree(a);
    }
}

/* ---- Coherent DMA memory: contiguous, zeroed frames ---- */
void *dma_alloc_coherent(struct device *dev, size_t size, dma_addr_t *handle, gfp_t gfp)
{
    unsigned long n = DIV_ROUND_UP(size, PAGE_SIZE);
    u64 phys;
    void *cpu;
    (void)dev;
    (void)gfp;
    phys = n == 1 ? waveos_frame_alloc() : waveos_frames_alloc_contiguous(n);
    if (!phys)
        return NULL;
    cpu = waveos_phys_to_virt(phys);
    memset(cpu, 0, n * PAGE_SIZE);
    *handle = phys;
    return cpu;
}

void dma_free_coherent(struct device *dev, size_t size, void *cpu, dma_addr_t handle)
{
    unsigned long n = DIV_ROUND_UP(size, PAGE_SIZE);
    (void)dev;
    (void)cpu;
    for (unsigned long i = 0; i < n; i++)
        waveos_frame_free(handle + i * PAGE_SIZE);
}

/* ---- Scatterlists ---- */
int sg_alloc_table(struct sg_table *t, unsigned int nents, gfp_t gfp)
{
    t->sgl = kcalloc(nents ? nents : 1, sizeof(struct scatterlist), gfp);
    if (!t->sgl)
        return -ENOMEM;
    sg_init_table(t->sgl, nents ? nents : 1);
    t->nents = t->orig_nents = nents;
    return 0;
}

void sg_free_table(struct sg_table *t)
{
    kfree(t->sgl);
    t->sgl = NULL;
    t->nents = t->orig_nents = 0;
}

int sg_alloc_table_from_pages_segment(struct sg_table *t, struct page **pages, unsigned int n, unsigned int offset,
                                      unsigned long size, unsigned int max_segment, gfp_t gfp)
{
    unsigned int segs = 0, i, j;
    struct scatterlist *sg;
    int ret;
    if (!n)
        return -EINVAL;
    max_segment = max_t(unsigned int, ALIGN_DOWN(max_segment, PAGE_SIZE), PAGE_SIZE);
    /* Count runs of physically consecutive pages. */
    for (i = 0; i < n; i = j) {
        unsigned long len = PAGE_SIZE;
        for (j = i + 1; j < n && pages[j]->phys == pages[j - 1]->phys + PAGE_SIZE && len + PAGE_SIZE <= max_segment; j++)
            len += PAGE_SIZE;
        segs++;
    }
    ret = sg_alloc_table(t, segs, gfp);
    if (ret)
        return ret;
    sg = t->sgl;
    for (i = 0; i < n; i = j) {
        unsigned long len = PAGE_SIZE;
        unsigned int off = i == 0 ? offset : 0;
        for (j = i + 1; j < n && pages[j]->phys == pages[j - 1]->phys + PAGE_SIZE && len + PAGE_SIZE <= max_segment; j++)
            len += PAGE_SIZE;
        len -= off;
        if (len > size)
            len = size;
        size -= len;
        sg_set_page(sg, pages[i], len, off);
        sg->dma_address = pages[i]->phys + off;
        sg->dma_length = len;
        sg = sg_next(sg);
    }
    return 0;
}

void __sg_page_iter_start(struct sg_page_iter *piter, struct scatterlist *sglist, unsigned int nents,
                          unsigned long pgoffset)
{
    piter->pg_advance = 0;
    piter->nents = nents;
    piter->sg = sglist;
    piter->sg_pgoffset = pgoffset;
}

static unsigned int sg_page_count(struct scatterlist *sg)
{
    return PAGE_ALIGN(sg->offset + sg->length) >> PAGE_SHIFT;
}

bool __sg_page_iter_next(struct sg_page_iter *piter)
{
    if (!piter->nents || !piter->sg)
        return false;
    piter->sg_pgoffset += piter->pg_advance;
    piter->pg_advance = 1;
    while (piter->sg_pgoffset >= sg_page_count(piter->sg)) {
        piter->sg_pgoffset -= sg_page_count(piter->sg);
        piter->sg = sg_next(piter->sg);
        if (!--piter->nents || !piter->sg)
            return false;
    }
    return true;
}

/* ---- Physical address ranges (nothing claims them on WaveOS) ---- */
struct resource iomem_resource = { .name = "PCI mem", .start = 0, .end = ~0ULL };
struct resource ioport_resource = { .name = "PCI IO", .start = 0, .end = 0xffff };
