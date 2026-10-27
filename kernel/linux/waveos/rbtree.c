// SPDX-License-Identifier: MIT
/*
 * WaveOS LinuxKPI: red-black tree rebalancing (see <waveos/rbtree.h>).
 *
 * The textbook algorithm with parent pointers. Every rotation reports to the
 * augment callbacks so subtree summaries stay correct.
 */
#include <waveos/rbtree.h>

#define RED 0UL
#define BLACK 1UL

typedef void (*rotate_fn)(struct rb_node *old, struct rb_node *new);

static void no_rotate(struct rb_node *old, struct rb_node *new) { (void)old; (void)new; }

static const struct rb_augment_callbacks no_augment = { NULL, NULL, no_rotate };

static inline bool is_black(const struct rb_node *n) { return !n || (n->__rb_parent_color & BLACK); }
static inline unsigned long color(const struct rb_node *n) { return n->__rb_parent_color & BLACK; }
static inline void set_parent(struct rb_node *n, struct rb_node *p)
{
    n->__rb_parent_color = (unsigned long)p | color(n);
}
static inline void set_color(struct rb_node *n, unsigned long c)
{
    n->__rb_parent_color = (n->__rb_parent_color & ~BLACK) | c;
}

/* Points parent's link (or the root) at new instead of old. */
static inline void change_child(struct rb_node *old, struct rb_node *new, struct rb_node *parent, struct rb_root *root)
{
    if (!parent)
        WRITE_ONCE(root->rb_node, new);
    else if (parent->rb_left == old)
        WRITE_ONCE(parent->rb_left, new);
    else
        WRITE_ONCE(parent->rb_right, new);
}

/* x's right child takes x's place; x becomes its left child. */
static void rotate_left(struct rb_node *x, struct rb_root *root, rotate_fn rotate)
{
    struct rb_node *y = x->rb_right, *p = rb_parent(x);
    x->rb_right = y->rb_left;
    if (y->rb_left)
        set_parent(y->rb_left, x);
    y->rb_left = x;
    set_parent(y, p);
    change_child(x, y, p, root);
    set_parent(x, y);
    rotate(x, y);
}

/* x's left child takes x's place; x becomes its right child. */
static void rotate_right(struct rb_node *x, struct rb_root *root, rotate_fn rotate)
{
    struct rb_node *y = x->rb_left, *p = rb_parent(x);
    x->rb_left = y->rb_right;
    if (y->rb_right)
        set_parent(y->rb_right, x);
    y->rb_right = x;
    set_parent(y, p);
    change_child(x, y, p, root);
    set_parent(x, y);
    rotate(x, y);
}

static void insert_fixup(struct rb_node *z, struct rb_root *root, rotate_fn rotate)
{
    struct rb_node *p;
    while ((p = rb_parent(z)) && !is_black(p)) {
        struct rb_node *g = rb_parent(p); /* a red node is never the root */
        if (p == g->rb_left) {
            struct rb_node *u = g->rb_right;
            if (!is_black(u)) {
                set_color(p, BLACK);
                set_color(u, BLACK);
                set_color(g, RED);
                z = g;
                continue;
            }
            if (z == p->rb_right) {
                rotate_left(p, root, rotate);
                z = p;
                p = rb_parent(z);
            }
            set_color(p, BLACK);
            set_color(g, RED);
            rotate_right(g, root, rotate);
        } else {
            struct rb_node *u = g->rb_left;
            if (!is_black(u)) {
                set_color(p, BLACK);
                set_color(u, BLACK);
                set_color(g, RED);
                z = g;
                continue;
            }
            if (z == p->rb_left) {
                rotate_right(p, root, rotate);
                z = p;
                p = rb_parent(z);
            }
            set_color(p, BLACK);
            set_color(g, RED);
            rotate_left(g, root, rotate);
        }
    }
    set_color(root->rb_node, BLACK);
}

/* x (possibly empty) under parent is one black short; restore the invariant. */
static void erase_fixup(struct rb_node *x, struct rb_node *parent, struct rb_root *root, rotate_fn rotate)
{
    while (x != root->rb_node && is_black(x)) {
        struct rb_node *w;
        if (x == parent->rb_left) {
            w = parent->rb_right;
            if (!is_black(w)) {
                set_color(w, BLACK);
                set_color(parent, RED);
                rotate_left(parent, root, rotate);
                w = parent->rb_right;
            }
            if (is_black(w->rb_left) && is_black(w->rb_right)) {
                set_color(w, RED);
                x = parent;
                parent = rb_parent(x);
                continue;
            }
            if (is_black(w->rb_right)) {
                set_color(w->rb_left, BLACK);
                set_color(w, RED);
                rotate_right(w, root, rotate);
                w = parent->rb_right;
            }
            set_color(w, color(parent));
            set_color(parent, BLACK);
            set_color(w->rb_right, BLACK);
            rotate_left(parent, root, rotate);
        } else {
            w = parent->rb_left;
            if (!is_black(w)) {
                set_color(w, BLACK);
                set_color(parent, RED);
                rotate_right(parent, root, rotate);
                w = parent->rb_left;
            }
            if (is_black(w->rb_left) && is_black(w->rb_right)) {
                set_color(w, RED);
                x = parent;
                parent = rb_parent(x);
                continue;
            }
            if (is_black(w->rb_left)) {
                set_color(w->rb_right, BLACK);
                set_color(w, RED);
                rotate_left(w, root, rotate);
                w = parent->rb_left;
            }
            set_color(w, color(parent));
            set_color(parent, BLACK);
            set_color(w->rb_left, BLACK);
            rotate_right(parent, root, rotate);
        }
        x = root->rb_node;
    }
    if (x)
        set_color(x, BLACK);
}

static void erase(struct rb_node *z, struct rb_root *root, const struct rb_augment_callbacks *aug)
{
    struct rb_node *x, *xparent;
    unsigned long removed;

    if (!z->rb_left || !z->rb_right) {
        x = z->rb_left ? z->rb_left : z->rb_right;
        xparent = rb_parent(z);
        removed = color(z);
        if (x)
            set_parent(x, xparent);
        change_child(z, x, xparent, root);
        if (aug->propagate && xparent)
            aug->propagate(xparent, NULL);
    } else {
        /* The successor y moves into z's place. */
        struct rb_node *y = z->rb_right, *zparent = rb_parent(z);
        while (y->rb_left)
            y = y->rb_left;
        removed = color(y);
        x = y->rb_right;
        if (y == z->rb_right) {
            xparent = y;
        } else {
            xparent = rb_parent(y);
            xparent->rb_left = x;
            if (x)
                set_parent(x, xparent);
            y->rb_right = z->rb_right;
            set_parent(z->rb_right, y);
        }
        y->rb_left = z->rb_left;
        set_parent(z->rb_left, y);
        y->__rb_parent_color = z->__rb_parent_color;
        change_child(z, y, zparent, root);
        if (aug->propagate) {
            aug->copy(z, y);
            if (xparent != y)
                aug->propagate(xparent, y);
            aug->propagate(y, NULL);
        }
    }
    if (removed == BLACK)
        erase_fixup(x, xparent, root, aug->rotate);
}

void rb_insert_color(struct rb_node *node, struct rb_root *root) { insert_fixup(node, root, no_rotate); }
void rb_erase(struct rb_node *node, struct rb_root *root) { erase(node, root, &no_augment); }
void rb_insert_augmented(struct rb_node *node, struct rb_root *root, const struct rb_augment_callbacks *aug)
{
    insert_fixup(node, root, aug->rotate);
}
void rb_erase_augmented(struct rb_node *node, struct rb_root *root, const struct rb_augment_callbacks *aug)
{
    erase(node, root, aug);
}

void rb_replace_node(struct rb_node *victim, struct rb_node *new, struct rb_root *root)
{
    struct rb_node *parent = rb_parent(victim);
    *new = *victim;
    if (victim->rb_left)
        set_parent(victim->rb_left, new);
    if (victim->rb_right)
        set_parent(victim->rb_right, new);
    change_child(victim, new, parent, root);
}

struct rb_node *rb_first(const struct rb_root *root)
{
    struct rb_node *n = root->rb_node;
    if (!n)
        return NULL;
    while (n->rb_left)
        n = n->rb_left;
    return n;
}

struct rb_node *rb_last(const struct rb_root *root)
{
    struct rb_node *n = root->rb_node;
    if (!n)
        return NULL;
    while (n->rb_right)
        n = n->rb_right;
    return n;
}

struct rb_node *rb_next(const struct rb_node *node)
{
    struct rb_node *parent;
    if (RB_EMPTY_NODE(node))
        return NULL;
    if (node->rb_right) {
        node = node->rb_right;
        while (node->rb_left)
            node = node->rb_left;
        return (struct rb_node *)node;
    }
    while ((parent = rb_parent(node)) && node == parent->rb_right)
        node = parent;
    return parent;
}

struct rb_node *rb_prev(const struct rb_node *node)
{
    struct rb_node *parent;
    if (RB_EMPTY_NODE(node))
        return NULL;
    if (node->rb_left) {
        node = node->rb_left;
        while (node->rb_right)
            node = node->rb_right;
        return (struct rb_node *)node;
    }
    while ((parent = rb_parent(node)) && node == parent->rb_left)
        node = parent;
    return parent;
}

/* The first node visited in post-order below node: keep descending, left first. */
static struct rb_node *deepest_left(const struct rb_node *node)
{
    for (;;) {
        if (node->rb_left)
            node = node->rb_left;
        else if (node->rb_right)
            node = node->rb_right;
        else
            return (struct rb_node *)node;
    }
}

struct rb_node *rb_first_postorder(const struct rb_root *root)
{
    return root->rb_node ? deepest_left(root->rb_node) : NULL;
}

struct rb_node *rb_next_postorder(const struct rb_node *node)
{
    struct rb_node *parent;
    if (!node)
        return NULL;
    parent = rb_parent(node);
    if (parent && node == parent->rb_left && parent->rb_right)
        return deepest_left(parent->rb_right);
    return parent;
}
