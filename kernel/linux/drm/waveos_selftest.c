// SPDX-License-Identifier: MIT
/*
 * WaveOS: self-test of the Linux compatibility layer, run by the kernel
 * tests (kernel/src/tests.rs). Exercises the WaveOS-written runtime the
 * drivers stand on — formatting, ID maps, red-black and interval trees,
 * sorting, fences — and a piece of DRM proper (CVT mode timings).
 * Returns 0, or the line of the first failed check.
 */
#include <linux/kernel.h>
#include <linux/list_sort.h>
#include <linux/rbtree.h>
#include <linux/slab.h>
#include <linux/sort.h>

#include <drm/drm_mm.h>
#include <drm/drm_modes.h>
#include <drm/drm_fourcc.h>
#include <linux/dma-fence.h>
#include <linux/idr.h>

#define CHECK(cond)                                                                                    \
    do {                                                                                               \
        if (!(cond)) {                                                                                 \
            pr_err("LinuxKPI self-test failed: %s (line %d)\n", #cond, __LINE__);                      \
            return __LINE__;                                                                           \
        }                                                                                              \
    } while (0)

static u32 rng = 2463534242u;
static u32 next_random(void)
{
    rng ^= rng << 13;
    rng ^= rng >> 17;
    rng ^= rng << 5;
    return rng;
}

static int test_format(void)
{
    char buf[96];
    u32 fourcc = DRM_FORMAT_XRGB8888;
    CHECK(snprintf(buf, sizeof(buf), "%d|%5u|%-4x|%#llx|%s|%.2s|%c", -42, 7u, 0xabu, 0x1234ULL, "ok", "abc", 'z') ==
          29);
    CHECK(!strcmp(buf, "-42|    7|ab  |0x1234|ok|ab|z"));
    CHECK(snprintf(buf, sizeof(buf), "%08lx %+d %zu", 0xbeefUL, 5, (size_t)12) == 14);
    CHECK(!strcmp(buf, "0000beef +5 12"));
    snprintf(buf, sizeof(buf), "%p4cc", &fourcc);
    CHECK(!strncmp(buf, "XR24 little-endian", 18));
    CHECK(snprintf(buf, 4, "%s", "truncate") == 8 && !strcmp(buf, "tru"));
    return 0;
}

static int test_idr(void)
{
    struct idr idr;
    int ids[200];
    void *p;
    int id;
    idr_init_base(&idr, 1);
    for (int i = 0; i < 200; i++) {
        ids[i] = idr_alloc(&idr, (void *)(unsigned long)(i + 1) /* any non-NULL tag */, 1, 0, GFP_KERNEL);
        CHECK(ids[i] == i + 1);
    }
    CHECK(idr_find(&idr, 57) == (void *)57UL);
    CHECK(idr_remove(&idr, 57) == (void *)57UL && !idr_find(&idr, 57));
    CHECK(idr_alloc(&idr, (void *)1, 1, 0, GFP_KERNEL) == 57); /* lowest free slot */
    /* A reserved (NULL) id is allocated, loads as NULL and can be filled in. */
    id = idr_alloc(&idr, NULL, 1, 0, GFP_KERNEL);
    CHECK(id == 201 && !idr_find(&idr, id));
    CHECK(!IS_ERR(idr_replace(&idr, (void *)9, id)) && idr_find(&idr, id) == (void *)9);
    CHECK(IS_ERR(idr_replace(&idr, (void *)9, 5000)));
    CHECK(idr_alloc(&idr, (void *)1, 1, 5, GFP_KERNEL) == -ENOSPC);
    id = 0;
    CHECK((p = idr_get_next(&idr, &id)) && id == 1);
    idr_destroy(&idr);
    return 0;
}

struct item {
    struct rb_node rb;
    struct list_head link;
    u32 key;
    unsigned int order;
};

static bool item_less(struct rb_node *a, const struct rb_node *b)
{
    return rb_entry(a, struct item, rb)->key < rb_entry(b, struct item, rb)->key;
}

/* Black height of the subtree, or -1 if a red-black rule is broken. */
static int black_height(struct rb_node *n)
{
    int l, r;
    bool black;
    if (!n)
        return 1;
    black = n->__rb_parent_color & 1;
    if (!black && ((n->rb_left && !(n->rb_left->__rb_parent_color & 1)) ||
                   (n->rb_right && !(n->rb_right->__rb_parent_color & 1))))
        return -1;
    if ((n->rb_left && rb_parent(n->rb_left) != n) || (n->rb_right && rb_parent(n->rb_right) != n))
        return -1;
    l = black_height(n->rb_left);
    r = black_height(n->rb_right);
    if (l < 0 || l != r)
        return -1;
    return l + black;
}

static int test_rbtree(void)
{
    const int n = 2000;
    struct item *items = kcalloc(n, sizeof(*items), GFP_KERNEL);
    struct rb_root root = RB_ROOT;
    struct rb_node *node;
    u32 last = 0;
    int count = 0;
    CHECK(items);
    for (int i = 0; i < n; i++) {
        items[i].key = next_random();
        rb_add(&items[i].rb, &root, item_less);
    }
    CHECK(black_height(root.rb_node) > 0);
    for (node = rb_first(&root); node; node = rb_next(node), count++) {
        CHECK(rb_entry(node, struct item, rb)->key >= last);
        last = rb_entry(node, struct item, rb)->key;
    }
    CHECK(count == n);
    for (int i = 0; i < n; i += 2)
        rb_erase(&items[i].rb, &root);
    CHECK(black_height(root.rb_node) > 0);
    count = 0;
    for (node = rb_last(&root); node; node = rb_prev(node))
        count++;
    CHECK(count == n / 2);
    kfree(items);
    return 0;
}

/* drm_mm stands on the augmented (interval) trees. */
static int test_drm_mm(void)
{
    const int n = 64;
    struct drm_mm mm;
    struct drm_mm_node *nodes = kcalloc(n, sizeof(*nodes), GFP_KERNEL);
    CHECK(nodes);
    drm_mm_init(&mm, 0, 1 << 20);
    for (int i = 0; i < n; i++)
        CHECK(!drm_mm_insert_node(&mm, &nodes[i], 4096 + (next_random() % 8) * 512));
    for (int i = 0; i < n; i += 3)
        drm_mm_remove_node(&nodes[i]);
    /* Holes are found again, and nothing overlaps. */
    for (int i = 0; i < n; i += 3)
        CHECK(!drm_mm_insert_node(&mm, &nodes[i], 4096));
    for (int i = 0; i < n; i++)
        for (int j = i + 1; j < n; j++)
            CHECK(nodes[i].start + nodes[i].size <= nodes[j].start || nodes[j].start + nodes[j].size <= nodes[i].start);
    for (int i = 0; i < n; i++)
        drm_mm_remove_node(&nodes[i]);
    CHECK(drm_mm_clean(&mm));
    drm_mm_takedown(&mm);
    kfree(nodes);
    return 0;
}

static int cmp_u32(const void *a, const void *b)
{
    u32 x = *(const u32 *)a, y = *(const u32 *)b;
    return x < y ? -1 : x > y;
}

static int cmp_items(void *priv, const struct list_head *a, const struct list_head *b)
{
    (void)priv;
    return (int)(list_entry(a, struct item, link)->key % 16) - (int)(list_entry(b, struct item, link)->key % 16);
}

static int test_sort(void)
{
    u32 v[500];
    struct item items[300];
    struct item *it, *prev = NULL;
    LIST_HEAD(list);
    for (int i = 0; i < 500; i++)
        v[i] = next_random() % 1000;
    sort(v, 500, sizeof(u32), cmp_u32, NULL);
    for (int i = 1; i < 500; i++)
        CHECK(v[i - 1] <= v[i]);
    for (int i = 0; i < 300; i++) {
        items[i].key = next_random();
        items[i].order = i;
        list_add_tail(&items[i].link, &list);
    }
    list_sort(NULL, &list, cmp_items);
    list_for_each_entry(it, &list, link) {
        /* Sorted by key % 16, and stable within equal keys. */
        if (prev)
            CHECK(prev->key % 16 < it->key % 16 || (prev->key % 16 == it->key % 16 && prev->order < it->order));
        prev = it;
    }
    return 0;
}

static const char *fence_name(struct dma_fence *f)
{
    (void)f;
    return "selftest";
}
static const struct dma_fence_ops fence_ops = { .get_driver_name = fence_name, .get_timeline_name = fence_name };

static void fence_cb(struct dma_fence *f, struct dma_fence_cb *cb)
{
    (void)f;
    *(int *)((char *)cb + sizeof(*cb)) += 1;
}

static int test_fence(void)
{
    static DEFINE_SPINLOCK(lock);
    struct dma_fence f;
    struct {
        struct dma_fence_cb cb;
        int calls;
    } cb = { .calls = 0 };
    dma_fence_init(&f, &fence_ops, &lock, dma_fence_context_alloc(1), 1);
    CHECK(!dma_fence_is_signaled(&f));
    CHECK(!dma_fence_add_callback(&f, &cb.cb, fence_cb));
    CHECK(dma_fence_wait_timeout(&f, false, 0) == 0);
    CHECK(!dma_fence_signal(&f));
    CHECK(cb.calls == 1 && dma_fence_is_signaled(&f));
    CHECK(dma_fence_signal(&f) == -EINVAL); /* once only */
    CHECK(dma_fence_wait_timeout(&f, false, 10) > 0);
    CHECK(dma_fence_add_callback(&f, &cb.cb, fence_cb) == -ENOENT);
    return 0;
}

/*
 * CVT 1920x1080 at 60 Hz: htotal 2576, vtotal 1120, and (DRM's integer
 * arithmetic, without the 250 kHz rounding of the cvt tool) 173106 kHz.
 */
static int test_cvt(void)
{
    struct drm_display_mode *m = drm_cvt_mode(NULL, 1920, 1080, 60, false, false, false);
    CHECK(m);
    CHECK(m->hdisplay == 1920 && m->vdisplay == 1080);
    CHECK(m->clock == 173106 && m->htotal == 2576 && m->vtotal == 1120);
    kfree(m);
    return 0;
}

int waveos_lkpi_selftest(void)
{
    int (*tests[])(void) = { test_format, test_idr, test_rbtree, test_drm_mm, test_sort, test_fence, test_cvt };
    for (unsigned int i = 0; i < ARRAY_SIZE(tests); i++) {
        int line = tests[i]();
        if (line)
            return line;
    }
    return 0;
}
