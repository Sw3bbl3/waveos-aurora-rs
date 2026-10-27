/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: pages, scatterlists, mappings, device memory and DMA.
 *
 * A struct page describes one 4 KiB physical frame (WaveOS's frame
 * allocator hands them out). Every frame is reachable through the kernel's
 * physical window, so page_address() always works; vmap() makes several
 * frames contiguous in the kernel virtual area. DMA addresses are physical
 * addresses: WaveOS programs no IOMMU.
 */
#ifndef _WAVEOS_MM_H
#define _WAVEOS_MM_H

#include <waveos/fs.h>

struct page {
    u64 phys;
    atomic_t refcount;
    unsigned long private;
    struct address_space *mapping;
    pgoff_t index;
    unsigned long flags;
    struct page *waveos_next; /* hash chain (kernel/linux/waveos/mem.c) */
    unsigned int waveos_order; /* allocated by alloc_pages with this order, + 1; 0 if not */
};
struct folio {
    struct page page;
};
typedef struct { unsigned long pgprot; } pgprot_t;
#define PAGE_KERNEL ((pgprot_t){ 0x63 })
#define PAGE_KERNEL_IO ((pgprot_t){ 0x7b })
#define PAGE_KERNEL_RO ((pgprot_t){ 0x61 })
#define pgprot_writecombine(p) (p)
#define pgprot_noncached(p) (p)
#define pgprot_decrypted(p) (p)
#define pgprot_val(p) ((p).pgprot)
#define __pgprot(v) ((pgprot_t){ (v) })
#define VM_MAP 0x4
#define VM_IOREMAP 0x1

/* Provided by WaveOS (kernel/src/linux). */
u64 waveos_frame_alloc(void);
void waveos_frame_free(u64 phys);
u64 waveos_frames_alloc_contiguous(unsigned long count);
void *waveos_phys_to_virt(u64 phys);
u64 waveos_virt_to_phys(const void *virt);
void *waveos_kmap_frames(const u64 *frames, unsigned long count);
void waveos_kunmap(void *virt, unsigned long count);
void *waveos_map_mmio(u64 phys, u64 size, bool write_combining);

struct page *waveos_page_for_phys(u64 phys);
struct page *alloc_pages(gfp_t gfp, unsigned int order);
#define alloc_page(gfp) alloc_pages(gfp, 0)
void __free_pages(struct page *page, unsigned int order);
#define __free_page(p) __free_pages(p, 0)
#define free_page(addr) __free_page(virt_to_page((void *)(addr)))
static inline unsigned long __get_free_page(gfp_t gfp) { struct page *p = alloc_page(gfp); return p ? (unsigned long)waveos_phys_to_virt(p->phys) : 0; }
#define get_zeroed_page(gfp) __get_free_page((gfp) | __GFP_ZERO)
#define __get_free_pages(gfp, order) ((unsigned long)page_address(alloc_pages(gfp, order)))
#define free_pages(addr, order) __free_pages(virt_to_page((void *)(addr)), order)
static inline phys_addr_t page_to_phys(const struct page *p) { return p->phys; }
static inline unsigned long page_to_pfn(const struct page *p) { return (unsigned long)(p->phys >> PAGE_SHIFT); }
static inline void *page_address(const struct page *p) { return p ? waveos_phys_to_virt(p->phys) : NULL; }
#define pfn_to_page(pfn) waveos_page_for_phys((u64)(pfn) << PAGE_SHIFT)
#define phys_to_page(phys) waveos_page_for_phys(phys)
#define virt_to_page(v) waveos_page_for_phys(waveos_virt_to_phys(v) & PAGE_MASK)
#define virt_to_phys(v) waveos_virt_to_phys(v)
#define phys_to_virt(p) waveos_phys_to_virt(p)
#define __pa(v) waveos_virt_to_phys((const void *)(v))
#define __va(p) waveos_phys_to_virt(p)
#define virt_addr_valid(v) true
#define is_vmalloc_addr(v) false
#define vmalloc_to_page(v) virt_to_page(v)
#define page_to_virt(p) page_address(p)
#define offset_in_page(p) ((unsigned long)(p) & ~PAGE_MASK)
static inline void get_page(struct page *p) { atomic_inc(&p->refcount); }
void put_page(struct page *p);
#define page_count(p) atomic_read(&(p)->refcount)
#define set_page_dirty(p) 0
#define set_page_dirty_lock(p) 0
#define mark_page_accessed(p) do { } while (0)
#define PageHighMem(p) false
#define page_folio(p) ((struct folio *)(p))
#define folio_page(f, n) (&(f)->page + (n))
#define folio_address(f) page_address(&(f)->page)
#define folio_put(f) put_page(&(f)->page)
#define folio_get(f) get_page(&(f)->page)
#define folio_mark_dirty(f) false
#define folio_mark_accessed(f) do { } while (0)
#define folio_nr_pages(f) 1
#define folio_size(f) PAGE_SIZE
#define PAGE_ALIGNED(addr) IS_ALIGNED((unsigned long)(addr), PAGE_SIZE)
#define folio_file_page(f, index) folio_page(f, 0)
#define folio_test_highmem(f) false
#define folio_pfn(f) page_to_pfn(&(f)->page)
#define kmap(p) page_address(p)
#define kunmap(p) do { } while (0)
#define kmap_local_page(p) page_address(p)
#define kunmap_local(v) do { } while (0)
#define kmap_atomic(p) page_address(p)
#define kunmap_atomic(v) do { } while (0)
#define kmap_local_folio(f, off) ((char *)folio_address(f) + (off))
#define flush_dcache_page(p) do { } while (0)
#define clear_page(addr) memset(addr, 0, PAGE_SIZE)
#define totalram_pages() 0UL
#define get_num_physpages() 0UL
#define si_meminfo(i) do { } while (0)
#define page_private(p) ((p)->private)
#define set_page_private(p, v) ((p)->private = (v))
#define pin_user_pages_fast(s, n, f, p) (-EFAULT)
#define unpin_user_pages(p, n) do { } while (0)
#define unpin_user_page(p) do { } while (0)

/* Several frames, contiguous in kernel virtual memory. */
void *vmap(struct page **pages, unsigned int count, unsigned long flags, pgprot_t prot);
void vunmap(const void *addr);
#define vm_map_ram(p, c, n) vmap(p, c, VM_MAP, PAGE_KERNEL)
#define vm_unmap_ram(a, c) vunmap(a)

/* ---- Folio batches and shmem: GEM's shmem paths are replaced on WaveOS ---- */
#define PAGEVEC_SIZE 31
struct folio_batch {
    unsigned char nr;
    struct folio *folios[PAGEVEC_SIZE];
};
static inline void folio_batch_init(struct folio_batch *b) { b->nr = 0; }
static inline unsigned int folio_batch_count(struct folio_batch *b) { return b->nr; }
static inline unsigned int folio_batch_space(struct folio_batch *b) { return PAGEVEC_SIZE - b->nr; }
static inline unsigned int folio_batch_add(struct folio_batch *b, struct folio *f) { b->folios[b->nr++] = f; return folio_batch_space(b); }
static inline void folio_batch_release(struct folio_batch *b)
{
    for (unsigned int i = 0; i < b->nr; i++)
        folio_put(b->folios[i]);
    b->nr = 0;
}
#define __folio_batch_release folio_batch_release
#define check_move_unevictable_folios(b) do { } while (0)
#define mapping_set_unevictable(m) do { } while (0)
#define mapping_clear_unevictable(m) do { } while (0)
#define mapping_gfp_mask(m) GFP_KERNEL
#define mapping_set_gfp_mask(m, g) do { } while (0)
#define mapping_gfp_constraint(m, g) (g)
#define shmem_read_folio_gfp(m, i, g) ((struct folio *)ERR_PTR(-ENOSYS))
#define shmem_read_mapping_page_gfp(m, i, g) ((struct page *)ERR_PTR(-ENOSYS))
#define shmem_read_mapping_page(m, i) ((struct page *)ERR_PTR(-ENOSYS))
#define shmem_truncate_range(i, s, e) do { } while (0)
struct file *shmem_file_setup(const char *name, loff_t size, unsigned long flags);
#define shmem_file_setup_with_mnt(mnt, name, size, flags) shmem_file_setup(name, size, flags)
#define shmem_get_folio(i, idx, w, f, sgp) (-ENOSYS)
#define VM_NORESERVE 0x00200000
#define invalidate_mapping_pages(m, s, e) 0
#define truncate_inode_pages(m, l) do { } while (0)

/* ---- Virtual memory areas (mmap from user space: not used on WaveOS) ---- */
struct vm_operations_struct;
struct vm_fault;
typedef unsigned int vm_fault_t;
struct mm_struct { int unused; };
struct vm_area_struct {
    unsigned long vm_start;
    unsigned long vm_end;
    unsigned long vm_pgoff;
    unsigned long vm_flags;
    pgprot_t vm_page_prot;
    const struct vm_operations_struct *vm_ops;
    void *vm_private_data;
    struct file *vm_file;
    struct mm_struct *vm_mm;
};
struct vm_operations_struct {
    void (*open)(struct vm_area_struct *vma);
    void (*close)(struct vm_area_struct *vma);
    vm_fault_t (*fault)(struct vm_fault *vmf);
    int (*access)(struct vm_area_struct *vma, unsigned long addr, void *buf, int len, int write);
};
struct vm_fault {
    struct vm_area_struct *vma;
    unsigned long address;
    pgoff_t pgoff;
    unsigned int flags;
};
#define VM_FAULT_OOM 0x0001
#define VM_FAULT_SIGBUS 0x0002
#define VM_FAULT_NOPAGE 0x0100
#define VM_READ 0x00000001
#define VM_WRITE 0x00000002
#define VM_EXEC 0x00000004
#define VM_SHARED 0x00000008
#define VM_MAYWRITE 0x00000020
#define VM_IO 0x00004000
#define VM_PFNMAP 0x00000400
#define VM_DONTEXPAND 0x00040000
#define VM_DONTDUMP 0x04000000
#define VM_MIXEDMAP 0x10000000
#define vm_flags_set(v, f) ((v)->vm_flags |= (f))
#define vm_flags_clear(v, f) ((v)->vm_flags &= ~(f))
#define vm_flags_mod(v, s, c) ((v)->vm_flags = ((v)->vm_flags | (s)) & ~(c))
#define vma_pages(v) (((v)->vm_end - (v)->vm_start) >> PAGE_SHIFT)
#define vm_get_page_prot(f) PAGE_KERNEL
#define vmf_insert_pfn(v, a, p) VM_FAULT_SIGBUS
#define vmf_insert_page(v, a, p) VM_FAULT_SIGBUS
#define remap_pfn_range(v, a, p, s, prot) (-ENOSYS)
#define vma_set_file(v, f) do { (v)->vm_file = (f); } while (0)
#define is_cow_mapping(f) false
#define vma_is_anonymous(v) false
#define current_mm() ((struct mm_struct *)NULL)
#define mmap_read_lock(mm) do { } while (0)
#define mmap_read_unlock(mm) do { } while (0)
#define MAP_SHARED 0x01
#define PROT_READ 0x1
#define PROT_WRITE 0x2

/* ---- Device registers ---- */
struct resource {
    resource_size_t start;
    resource_size_t end;
    const char *name;
    unsigned long flags;
    struct resource *parent, *sibling, *child;
};
extern struct resource iomem_resource;
extern struct resource ioport_resource;
#define IORESOURCE_IO 0x00000100
#define IORESOURCE_MEM 0x00000200
#define IORESOURCE_PREFETCH 0x00002000
#define IORESOURCE_MEM_64 0x00100000
#define resource_size(r) ((r)->end - (r)->start + 1)
#define DEFINE_RES_MEM(s, sz) { .start = (s), .end = (s) + (sz) - 1, .flags = IORESOURCE_MEM }
#define request_mem_region(s, n, name) ((struct resource *)1)
#define release_mem_region(s, n) do { } while (0)
#define devm_request_mem_region(d, s, n, name) ((struct resource *)1)
static inline void __iomem *ioremap(phys_addr_t phys, size_t size) { return waveos_map_mmio(phys, size, false); }
static inline void __iomem *ioremap_wc(phys_addr_t phys, size_t size) { return waveos_map_mmio(phys, size, true); }
#define ioremap_cache ioremap
#define ioremap_uc ioremap
#define ioremap_np ioremap
#define iounmap(a) do { (void)(a); } while (0)
#define devm_ioremap(d, p, s) ioremap(p, s)
#define devm_ioremap_wc(d, p, s) ioremap_wc(p, s)
#define memremap(p, s, f) waveos_phys_to_virt(p)
#define memunmap(a) do { } while (0)
#define MEMREMAP_WB 1
#define MEMREMAP_WC 4
#define arch_phys_wc_add(b, s) 0
#define arch_phys_wc_del(h) do { } while (0)
#define arch_io_reserve_memtype_wc(s, sz) 0
#define arch_io_free_memtype_wc(s, sz) do { } while (0)
static inline u8 readb(const volatile void __iomem *a) { return *(const volatile u8 *)a; }
static inline u16 readw(const volatile void __iomem *a) { return *(const volatile u16 *)a; }
static inline u32 readl(const volatile void __iomem *a) { return *(const volatile u32 *)a; }
static inline u64 readq(const volatile void __iomem *a) { return *(const volatile u64 *)a; }
static inline void writeb(u8 v, volatile void __iomem *a) { *(volatile u8 *)a = v; }
static inline void writew(u16 v, volatile void __iomem *a) { *(volatile u16 *)a = v; }
static inline void writel(u32 v, volatile void __iomem *a) { *(volatile u32 *)a = v; }
static inline void writeq(u64 v, volatile void __iomem *a) { *(volatile u64 *)a = v; }
#define readb_relaxed readb
#define readw_relaxed readw
#define readl_relaxed readl
#define writel_relaxed writel
#define ioread8 readb
#define ioread16 readw
#define ioread32 readl
#define iowrite8 writeb
#define iowrite16 writew
#define iowrite32 writel
#define __raw_readl readl
#define __raw_writel writel
#define memcpy_toio(d, s, n) memcpy((void *)(d), s, n)
#define memcpy_fromio(d, s, n) memcpy(d, (const void *)(s), n)
#define memset_io(d, c, n) memset((void *)(d), c, n)
static inline u8 inb(u16 port) { u8 v; __asm__ __volatile__("inb %1, %0" : "=a"(v) : "Nd"(port)); return v; }
static inline u16 inw(u16 port) { u16 v; __asm__ __volatile__("inw %1, %0" : "=a"(v) : "Nd"(port)); return v; }
static inline u32 inl(u16 port) { u32 v; __asm__ __volatile__("inl %1, %0" : "=a"(v) : "Nd"(port)); return v; }
static inline void outb(u8 v, u16 port) { __asm__ __volatile__("outb %0, %1" : : "a"(v), "Nd"(port)); }
static inline void outw(u16 v, u16 port) { __asm__ __volatile__("outw %0, %1" : : "a"(v), "Nd"(port)); }
static inline void outl(u32 v, u16 port) { __asm__ __volatile__("outl %0, %1" : : "a"(v), "Nd"(port)); }

/* ---- Scatterlists ---- */
struct scatterlist {
    struct page *page;
    unsigned int offset;
    unsigned int length;
    dma_addr_t dma_address;
    unsigned int dma_length;
    bool end;
};
struct sg_table {
    struct scatterlist *sgl;
    unsigned int nents;
    unsigned int orig_nents;
};
#define sg_page(sg) ((sg)->page)
#define sg_dma_address(sg) ((sg)->dma_address)
#define sg_dma_len(sg) ((sg)->dma_length)
#define sg_virt(sg) ((char *)page_address((sg)->page) + (sg)->offset)
#define sg_phys(sg) (page_to_phys((sg)->page) + (sg)->offset)
#define sg_is_last(sg) ((sg)->end)
#define sg_mark_end(sg) ((sg)->end = true)
static inline struct scatterlist *sg_next(struct scatterlist *sg) { return sg->end ? NULL : sg + 1; }
static inline void sg_set_page(struct scatterlist *sg, struct page *page, unsigned int len, unsigned int offset)
{ sg->page = page; sg->offset = offset; sg->length = len; }
static inline void sg_init_table(struct scatterlist *sgl, unsigned int n)
{ memset(sgl, 0, sizeof(*sgl) * n); if (n) sgl[n - 1].end = true; }
static inline void sg_init_one(struct scatterlist *sg, const void *buf, unsigned int len)
{
    sg_init_table(sg, 1);
    sg->page = virt_to_page(buf);
    sg->offset = offset_in_page(buf);
    sg->length = len;
    sg->dma_address = waveos_virt_to_phys(buf);
    sg->dma_length = len;
}
#define for_each_sg(sglist, sg, nr, i) for ((i) = 0, (sg) = (sglist); (i) < (nr); (i)++, (sg) = sg_next(sg))
#define for_each_sgtable_sg(sgt, sg, i) for_each_sg((sgt)->sgl, sg, (sgt)->orig_nents, i)
#define for_each_sgtable_dma_sg(sgt, sg, i) for_each_sg((sgt)->sgl, sg, (sgt)->nents, i)
int sg_alloc_table(struct sg_table *t, unsigned int nents, gfp_t gfp);
void sg_free_table(struct sg_table *t);
int sg_alloc_table_from_pages_segment(struct sg_table *t, struct page **pages, unsigned int n, unsigned int offset,
                                      unsigned long size, unsigned int max_segment, gfp_t gfp);
#define sg_alloc_table_from_pages(t, p, n, o, s, g) sg_alloc_table_from_pages_segment(t, p, n, o, s, UINT_MAX, g)
struct sg_page_iter {
    struct scatterlist *sg;
    unsigned int sg_pgoffset;
    unsigned int nents;
    int pg_advance;
};
struct sg_dma_page_iter {
    struct sg_page_iter base;
};
bool __sg_page_iter_next(struct sg_page_iter *piter);
void __sg_page_iter_start(struct sg_page_iter *piter, struct scatterlist *sglist, unsigned int nents, unsigned long pgoffset);
#define sg_page_iter_page(piter) (pfn_to_page(page_to_pfn((piter)->sg->page) + (piter)->sg_pgoffset))
#define for_each_sg_page(sglist, piter, nents, pgoffset) \
    for (__sg_page_iter_start((piter), (sglist), (nents), (pgoffset)); __sg_page_iter_next(piter);)
#define for_each_sgtable_page(sgt, piter, pgoffset) for_each_sg_page((sgt)->sgl, piter, (sgt)->orig_nents, pgoffset)
#define sg_page_iter_dma_address(dpiter) (page_to_phys((dpiter)->base.sg->page) + ((dma_addr_t)(dpiter)->base.sg_pgoffset << PAGE_SHIFT))
#define for_each_sgtable_dma_page(sgt, dma_iter, pgoffset) for_each_sg_page((sgt)->sgl, &(dma_iter)->base, (sgt)->nents, pgoffset)

/* ---- DMA: physical addresses, coherent x86 memory ---- */
enum dma_data_direction { DMA_BIDIRECTIONAL = 0, DMA_TO_DEVICE = 1, DMA_FROM_DEVICE = 2, DMA_NONE = 3 };
#define DMA_BIT_MASK(n) (((n) == 64) ? ~0ULL : ((1ULL << (n)) - 1))
#define DMA_MAPPING_ERROR (~(dma_addr_t)0)
#define DMA_ATTR_SKIP_CPU_SYNC (1UL << 5)
#define DMA_ATTR_WRITE_COMBINE (1UL << 2)
#define DMA_ATTR_NO_KERNEL_MAPPING (1UL << 4)
static inline int dma_set_mask(struct device *dev, u64 mask) { dev->dma_mask_storage = mask; dev->dma_mask = &dev->dma_mask_storage; return 0; }
static inline int dma_set_coherent_mask(struct device *dev, u64 mask) { dev->coherent_dma_mask = mask; return 0; }
static inline int dma_set_mask_and_coherent(struct device *dev, u64 mask) { dma_set_mask(dev, mask); return dma_set_coherent_mask(dev, mask); }
#define dma_get_mask(dev) DMA_BIT_MASK(64)
#define dma_mapping_error(dev, addr) ((addr) == DMA_MAPPING_ERROR)
static inline dma_addr_t dma_map_single(struct device *d, void *p, size_t s, enum dma_data_direction dir)
{ (void)d; (void)s; (void)dir; return waveos_virt_to_phys(p); }
#define dma_unmap_single(d, a, s, dir) do { } while (0)
static inline dma_addr_t dma_map_page(struct device *d, struct page *p, size_t off, size_t s, enum dma_data_direction dir)
{ (void)d; (void)s; (void)dir; return p->phys + off; }
#define dma_unmap_page(d, a, s, dir) do { } while (0)
#define dma_map_page_attrs(d, p, o, s, dir, a) dma_map_page(d, p, o, s, dir)
#define dma_unmap_page_attrs(d, a, s, dir, at) do { } while (0)
static inline int dma_map_sgtable(struct device *d, struct sg_table *sgt, enum dma_data_direction dir, unsigned long attrs)
{
    struct scatterlist *sg;
    unsigned int i;
    (void)d; (void)dir; (void)attrs;
    for_each_sgtable_sg(sgt, sg, i) { sg->dma_address = sg_phys(sg); sg->dma_length = sg->length; }
    sgt->nents = sgt->orig_nents;
    return 0;
}
#define dma_unmap_sgtable(d, sgt, dir, a) do { } while (0)
static inline int dma_map_sg(struct device *d, struct scatterlist *sgl, int n, enum dma_data_direction dir)
{
    struct scatterlist *sg;
    int i;
    (void)d; (void)dir;
    for_each_sg(sgl, sg, n, i) { sg->dma_address = sg_phys(sg); sg->dma_length = sg->length; }
    return n;
}
#define dma_unmap_sg(d, sg, n, dir) do { } while (0)
#define dma_sync_sgtable_for_device(d, s, dir) do { } while (0)
#define dma_sync_sgtable_for_cpu(d, s, dir) do { } while (0)
#define dma_sync_single_for_device(d, a, s, dir) do { } while (0)
#define dma_sync_single_for_cpu(d, a, s, dir) do { } while (0)
void *dma_alloc_coherent(struct device *dev, size_t size, dma_addr_t *handle, gfp_t gfp);
void dma_free_coherent(struct device *dev, size_t size, void *cpu, dma_addr_t handle);
#define dma_alloc_attrs(d, s, h, g, a) dma_alloc_coherent(d, s, h, g)
#define dma_free_attrs(d, s, c, h, a) dma_free_coherent(d, s, c, h)
#define dma_alloc_wc dma_alloc_coherent
#define dma_free_wc dma_free_coherent
#define dmam_alloc_coherent(d, s, h, g) dma_alloc_coherent(d, s, h, g)
#define dma_max_mapping_size(d) SIZE_MAX
#define dma_get_sgtable(d, sgt, c, h, s) (-ENOSYS)
#define dma_mmap_coherent(d, v, c, h, s) (-ENOSYS)
#define dma_mmap_wc dma_mmap_coherent
#define dma_addressing_limited(d) false
#define dev_is_dma_coherent(d) true
#define set_dma_ops(d, o) do { } while (0)
#define dma_is_direct(o) true
#define dma_get_max_seg_size(d) UINT_MAX
#define dma_set_max_seg_size(d, s) 0

/* ---- iosys_map: a CPU pointer to system or device memory ---- */
struct iosys_map {
    union {
        void __iomem *vaddr_iomem;
        void *vaddr;
    };
    bool is_iomem;
};
#define IOSYS_MAP_INIT_VADDR(v) { .vaddr = (v), .is_iomem = false }
#define IOSYS_MAP_INIT_VADDR_IOMEM(v) { .vaddr_iomem = (v), .is_iomem = true }
#define IOSYS_MAP_INIT_OFFSET(map, off) ({ struct iosys_map __m = *(map); iosys_map_incr(&__m, off); __m; })
static inline void iosys_map_set_vaddr(struct iosys_map *m, void *v) { m->vaddr = v; m->is_iomem = false; }
static inline void iosys_map_set_vaddr_iomem(struct iosys_map *m, void __iomem *v) { m->vaddr_iomem = v; m->is_iomem = true; }
static inline bool iosys_map_is_equal(const struct iosys_map *a, const struct iosys_map *b)
{ return a->is_iomem == b->is_iomem && a->vaddr == b->vaddr; }
static inline bool iosys_map_is_null(const struct iosys_map *m) { return !m->vaddr; }
static inline bool iosys_map_is_set(const struct iosys_map *m) { return !iosys_map_is_null(m); }
static inline void iosys_map_clear(struct iosys_map *m) { m->vaddr = NULL; m->is_iomem = false; }
static inline void iosys_map_incr(struct iosys_map *m, size_t incr) { m->vaddr = (char *)m->vaddr + incr; }
static inline void iosys_map_memcpy_to(struct iosys_map *dst, size_t off, const void *src, size_t len)
{ memcpy((char *)dst->vaddr + off, src, len); }
static inline void iosys_map_memcpy_from(void *dst, const struct iosys_map *src, size_t off, size_t len)
{ memcpy(dst, (const char *)src->vaddr + off, len); }
static inline void iosys_map_memset(struct iosys_map *dst, size_t off, int v, size_t len)
{ memset((char *)dst->vaddr + off, v, len); }
#define iosys_map_rd(map, off, type) ({ type __v; memcpy(&__v, (const char *)(map)->vaddr + (off), sizeof(type)); __v; })
#define iosys_map_wr(map, off, type, val) do { type __v = (val); memcpy((char *)(map)->vaddr + (off), &__v, sizeof(type)); } while (0)
#define iosys_map_rd_field(map, off, struct_type, field) iosys_map_rd(map, (off) + offsetof(struct_type, field), typeof_member(struct_type, field))
#define iosys_map_wr_field(map, off, struct_type, field, val) iosys_map_wr(map, (off) + offsetof(struct_type, field), typeof_member(struct_type, field), val)

#endif
