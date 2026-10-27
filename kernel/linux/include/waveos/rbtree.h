/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: red-black trees, augmented trees and interval trees.
 *
 * Written for WaveOS against the documented Linux interface (the layout of
 * struct rb_node is part of that interface: callers embed nodes and walk
 * rb_left/rb_right themselves). The colour lives in bit 0 of the parent
 * pointer, so an unlinked node points at itself.
 *
 * Augmented trees keep a per-node summary of their subtree. The caller
 * updates the summaries along the insertion path before linking; the tree
 * calls back into the caller when rotations and erasure change subtrees:
 *   propagate(n, stop)  recompute summaries from n up to (not including) stop
 *   copy(old, new)      new takes old's place: copy its summary
 *   rotate(old, new)    new becomes the parent of old: new takes old's
 *                       summary and old is recomputed
 */
#ifndef _WAVEOS_RBTREE_H
#define _WAVEOS_RBTREE_H

#include <waveos/kernel.h>

struct rb_node {
    unsigned long __rb_parent_color;
    struct rb_node *rb_right;
    struct rb_node *rb_left;
} __attribute__((aligned(sizeof(long))));
struct rb_root {
    struct rb_node *rb_node;
};
struct rb_root_cached {
    struct rb_root rb_root;
    struct rb_node *rb_leftmost;
};
struct rb_augment_callbacks {
    void (*propagate)(struct rb_node *node, struct rb_node *stop);
    void (*copy)(struct rb_node *old, struct rb_node *new);
    void (*rotate)(struct rb_node *old, struct rb_node *new);
};

#define RB_ROOT ((struct rb_root){ NULL })
#define RB_ROOT_CACHED ((struct rb_root_cached){ { NULL }, NULL })
#define rb_parent(r) ((struct rb_node *)((r)->__rb_parent_color & ~3UL))
#define rb_entry(ptr, type, member) container_of(ptr, type, member)
#define rb_entry_safe(ptr, type, member) \
    ({ typeof(ptr) ____ptr = (ptr); ____ptr ? rb_entry(____ptr, type, member) : NULL; })
#define RB_EMPTY_ROOT(root) (READ_ONCE((root)->rb_node) == NULL)
#define RB_EMPTY_NODE(node) ((node)->__rb_parent_color == (unsigned long)(node))
#define RB_CLEAR_NODE(node) ((node)->__rb_parent_color = (unsigned long)(node))
#define rb_first_cached(root) ((root)->rb_leftmost)

static inline void rb_link_node(struct rb_node *node, struct rb_node *parent, struct rb_node **link)
{
    node->__rb_parent_color = (unsigned long)parent; /* red */
    node->rb_left = node->rb_right = NULL;
    *link = node;
}
#define rb_link_node_rcu rb_link_node

void rb_insert_color(struct rb_node *node, struct rb_root *root);
void rb_erase(struct rb_node *node, struct rb_root *root);
void rb_insert_augmented(struct rb_node *node, struct rb_root *root, const struct rb_augment_callbacks *aug);
void rb_erase_augmented(struct rb_node *node, struct rb_root *root, const struct rb_augment_callbacks *aug);
void rb_replace_node(struct rb_node *victim, struct rb_node *new, struct rb_root *root);
struct rb_node *rb_first(const struct rb_root *root);
struct rb_node *rb_last(const struct rb_root *root);
struct rb_node *rb_next(const struct rb_node *node);
struct rb_node *rb_prev(const struct rb_node *node);
struct rb_node *rb_first_postorder(const struct rb_root *root);
struct rb_node *rb_next_postorder(const struct rb_node *node);

static inline void rb_insert_color_cached(struct rb_node *node, struct rb_root_cached *root, bool leftmost)
{
    if (leftmost)
        root->rb_leftmost = node;
    rb_insert_color(node, &root->rb_root);
}
static inline struct rb_node *rb_erase_cached(struct rb_node *node, struct rb_root_cached *root)
{
    struct rb_node *leftmost = NULL;
    if (root->rb_leftmost == node)
        leftmost = root->rb_leftmost = rb_next(node);
    rb_erase(node, &root->rb_root);
    return leftmost;
}
static inline void rb_insert_augmented_cached(struct rb_node *node, struct rb_root_cached *root, bool leftmost,
                                              const struct rb_augment_callbacks *aug)
{
    if (leftmost)
        root->rb_leftmost = node;
    rb_insert_augmented(node, &root->rb_root, aug);
}
static inline void rb_erase_augmented_cached(struct rb_node *node, struct rb_root_cached *root,
                                             const struct rb_augment_callbacks *aug)
{
    if (root->rb_leftmost == node)
        root->rb_leftmost = rb_next(node);
    rb_erase_augmented(node, &root->rb_root, aug);
}

static inline void rb_add_cached(struct rb_node *node, struct rb_root_cached *tree,
                                 bool (*less)(struct rb_node *, const struct rb_node *))
{
    struct rb_node **link = &tree->rb_root.rb_node, *parent = NULL;
    bool leftmost = true;
    while (*link) {
        parent = *link;
        if (less(node, parent)) {
            link = &parent->rb_left;
        } else {
            link = &parent->rb_right;
            leftmost = false;
        }
    }
    rb_link_node(node, parent, link);
    rb_insert_color_cached(node, tree, leftmost);
}
static inline void rb_add(struct rb_node *node, struct rb_root *tree,
                          bool (*less)(struct rb_node *, const struct rb_node *))
{
    struct rb_node **link = &tree->rb_node, *parent = NULL;
    while (*link) {
        parent = *link;
        link = less(node, parent) ? &parent->rb_left : &parent->rb_right;
    }
    rb_link_node(node, parent, link);
    rb_insert_color(node, tree);
}
static inline struct rb_node *rb_find(const void *key, const struct rb_root *tree,
                                      int (*cmp)(const void *key, const struct rb_node *))
{
    struct rb_node *node = tree->rb_node;
    while (node) {
        int c = cmp(key, node);
        if (c < 0)
            node = node->rb_left;
        else if (c > 0)
            node = node->rb_right;
        else
            return node;
    }
    return NULL;
}

#define rbtree_postorder_for_each_entry_safe(pos, n, root, field)                                      \
    for (pos = rb_entry_safe(rb_first_postorder(root), typeof(*pos), field);                           \
         pos && ({ n = rb_entry_safe(rb_next_postorder(&pos->field), typeof(*pos), field); 1; });      \
         pos = n)

/*
 * Callbacks for a summary that is the maximum of a per-node value over the
 * subtree. compute(n, exit) recomputes n's summary and, when exit is set,
 * reports whether it was already correct (so propagation can stop).
 */
#define RB_DECLARE_CALLBACKS_MAX(RBSTATIC, RBNAME, RBSTRUCT, RBFIELD, RBTYPE, RBAUGMENTED, RBCOMPUTE)   \
    static inline bool RBNAME##_compute_max(RBSTRUCT *node, bool exit)                                 \
    {                                                                                                  \
        RBTYPE max = RBCOMPUTE(node);                                                                  \
        if (node->RBFIELD.rb_left) {                                                                   \
            RBSTRUCT *c = rb_entry(node->RBFIELD.rb_left, RBSTRUCT, RBFIELD);                          \
            if (c->RBAUGMENTED > max)                                                                  \
                max = c->RBAUGMENTED;                                                                  \
        }                                                                                              \
        if (node->RBFIELD.rb_right) {                                                                  \
            RBSTRUCT *c = rb_entry(node->RBFIELD.rb_right, RBSTRUCT, RBFIELD);                         \
            if (c->RBAUGMENTED > max)                                                                  \
                max = c->RBAUGMENTED;                                                                  \
        }                                                                                              \
        if (exit && node->RBAUGMENTED == max)                                                          \
            return true;                                                                               \
        node->RBAUGMENTED = max;                                                                       \
        return false;                                                                                  \
    }                                                                                                  \
    RB_DECLARE_CALLBACKS(RBSTATIC, RBNAME, RBSTRUCT, RBFIELD, RBAUGMENTED, RBNAME##_compute_max)

#define RB_DECLARE_CALLBACKS(RBSTATIC, RBNAME, RBSTRUCT, RBFIELD, RBAUGMENTED, RBCOMPUTE)              \
    static inline void RBNAME##_propagate(struct rb_node *rb, struct rb_node *stop)                    \
    {                                                                                                  \
        for (; rb != stop; rb = rb_parent(rb))                                                         \
            if (RBCOMPUTE(rb_entry(rb, RBSTRUCT, RBFIELD), true))                                      \
                break;                                                                                 \
    }                                                                                                  \
    static inline void RBNAME##_copy(struct rb_node *rb_old, struct rb_node *rb_new)                   \
    {                                                                                                  \
        rb_entry(rb_new, RBSTRUCT, RBFIELD)->RBAUGMENTED = rb_entry(rb_old, RBSTRUCT, RBFIELD)->RBAUGMENTED; \
    }                                                                                                  \
    static void RBNAME##_rotate(struct rb_node *rb_old, struct rb_node *rb_new)                        \
    {                                                                                                  \
        RBNAME##_copy(rb_old, rb_new);                                                                 \
        RBCOMPUTE(rb_entry(rb_old, RBSTRUCT, RBFIELD), false);                                         \
    }                                                                                                  \
    RBSTATIC const struct rb_augment_callbacks RBNAME = {                                              \
        .propagate = RBNAME##_propagate,                                                               \
        .copy = RBNAME##_copy,                                                                         \
        .rotate = RBNAME##_rotate,                                                                     \
    };

/*
 * Interval trees: an augmented tree ordered by start whose summary is the
 * largest end ("last") in each subtree, so overlap queries skip subtrees
 * that end before the query begins.
 */
#define INTERVAL_TREE_DEFINE(ITSTRUCT, ITRB, ITTYPE, ITSUBTREE, ITSTART, ITLAST, ITSTATIC, ITPREFIX)   \
    RB_DECLARE_CALLBACKS_MAX(static, ITPREFIX##_augment, ITSTRUCT, ITRB, ITTYPE, ITSUBTREE, ITLAST)    \
                                                                                                       \
    ITSTATIC void ITPREFIX##_insert(ITSTRUCT *node, struct rb_root_cached *root)                       \
    {                                                                                                  \
        struct rb_node **link = &root->rb_root.rb_node, *rb_parent = NULL;                             \
        ITTYPE start = ITSTART(node), last = ITLAST(node);                                             \
        bool leftmost = true;                                                                          \
        while (*link) {                                                                                \
            ITSTRUCT *parent;                                                                          \
            rb_parent = *link;                                                                         \
            parent = rb_entry(rb_parent, ITSTRUCT, ITRB);                                              \
            if (parent->ITSUBTREE < last)                                                              \
                parent->ITSUBTREE = last;                                                              \
            if (start < ITSTART(parent)) {                                                             \
                link = &parent->ITRB.rb_left;                                                          \
            } else {                                                                                   \
                link = &parent->ITRB.rb_right;                                                         \
                leftmost = false;                                                                      \
            }                                                                                          \
        }                                                                                              \
        node->ITSUBTREE = last;                                                                        \
        rb_link_node(&node->ITRB, rb_parent, link);                                                    \
        rb_insert_augmented_cached(&node->ITRB, root, leftmost, &ITPREFIX##_augment);                  \
    }                                                                                                  \
                                                                                                       \
    ITSTATIC void ITPREFIX##_remove(ITSTRUCT *node, struct rb_root_cached *root)                       \
    {                                                                                                  \
        rb_erase_augmented_cached(&node->ITRB, root, &ITPREFIX##_augment);                             \
    }                                                                                                  \
                                                                                                       \
    /* The leftmost node in node's subtree that overlaps [start, last]. */                             \
    static ITSTRUCT *ITPREFIX##_subtree_search(ITSTRUCT *node, ITTYPE start, ITTYPE last)              \
    {                                                                                                  \
        for (;;) {                                                                                     \
            if (node->ITRB.rb_left) {                                                                  \
                ITSTRUCT *left = rb_entry(node->ITRB.rb_left, ITSTRUCT, ITRB);                         \
                if (start <= left->ITSUBTREE) {                                                        \
                    node = left;                                                                       \
                    continue;                                                                          \
                }                                                                                      \
            }                                                                                          \
            if (ITSTART(node) > last)                                                                  \
                return NULL;                                                                           \
            if (start <= ITLAST(node))                                                                 \
                return node;                                                                           \
            if (!node->ITRB.rb_right)                                                                  \
                return NULL;                                                                           \
            node = rb_entry(node->ITRB.rb_right, ITSTRUCT, ITRB);                                      \
            if (start > node->ITSUBTREE)                                                               \
                return NULL;                                                                           \
        }                                                                                              \
    }                                                                                                  \
                                                                                                       \
    ITSTATIC ITSTRUCT *ITPREFIX##_iter_first(struct rb_root_cached *root, ITTYPE start, ITTYPE last)   \
    {                                                                                                  \
        ITSTRUCT *node, *leftmost;                                                                     \
        if (!root->rb_root.rb_node)                                                                    \
            return NULL;                                                                               \
        node = rb_entry(root->rb_root.rb_node, ITSTRUCT, ITRB);                                        \
        if (node->ITSUBTREE < start)                                                                   \
            return NULL;                                                                               \
        leftmost = rb_entry(root->rb_leftmost, ITSTRUCT, ITRB);                                        \
        if (ITSTART(leftmost) > last)                                                                  \
            return NULL;                                                                               \
        return ITPREFIX##_subtree_search(node, start, last);                                           \
    }                                                                                                  \
                                                                                                       \
    ITSTATIC ITSTRUCT *ITPREFIX##_iter_next(ITSTRUCT *node, ITTYPE start, ITTYPE last)                 \
    {                                                                                                  \
        struct rb_node *rb = node->ITRB.rb_right, *prev;                                               \
        for (;;) {                                                                                     \
            /* Search the right subtree, if it can hold an overlap. */                                 \
            if (rb) {                                                                                  \
                ITSTRUCT *right = rb_entry(rb, ITSTRUCT, ITRB);                                        \
                if (start <= right->ITSUBTREE)                                                         \
                    return ITPREFIX##_subtree_search(right, start, last);                              \
            }                                                                                          \
            /* Otherwise climb until we arrive from a left child. */                                   \
            do {                                                                                       \
                rb = rb_parent(&node->ITRB);                                                           \
                if (!rb)                                                                               \
                    return NULL;                                                                       \
                prev = &node->ITRB;                                                                    \
                node = rb_entry(rb, ITSTRUCT, ITRB);                                                   \
                rb = node->ITRB.rb_right;                                                              \
            } while (prev == rb);                                                                      \
            if (last < ITSTART(node))                                                                  \
                return NULL;                                                                           \
            if (start <= ITLAST(node))                                                                 \
                return node;                                                                           \
        }                                                                                              \
    }

#endif
