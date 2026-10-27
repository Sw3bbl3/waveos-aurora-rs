/* SPDX-License-Identifier: MIT */
/* WaveOS LinuxKPI: lock-free singly linked lists (llist). */
#ifndef _WAVEOS_LLIST_H
#define _WAVEOS_LLIST_H

#include <waveos/kernel.h>

struct llist_node {
    struct llist_node *next;
};
struct llist_head {
    struct llist_node *first;
};
#define LLIST_HEAD_INIT(name) { NULL }
#define LLIST_HEAD(name) struct llist_head name = LLIST_HEAD_INIT(name)
static inline void init_llist_head(struct llist_head *l) { l->first = NULL; }
static inline void init_llist_node(struct llist_node *n) { n->next = n; }
static inline bool llist_on_list(const struct llist_node *n) { return n->next != n; }
static inline bool llist_empty(const struct llist_head *h) { return READ_ONCE(h->first) == NULL; }
static inline bool llist_add_batch(struct llist_node *first, struct llist_node *last, struct llist_head *head)
{
    struct llist_node *old = READ_ONCE(head->first);
    do {
        last->next = old;
    } while (!__atomic_compare_exchange_n(&head->first, &old, first, false, __ATOMIC_RELEASE, __ATOMIC_RELAXED));
    return old == NULL;
}
static inline bool llist_add(struct llist_node *n, struct llist_head *head) { return llist_add_batch(n, n, head); }
static inline struct llist_node *llist_del_all(struct llist_head *head)
{ return __atomic_exchange_n(&head->first, NULL, __ATOMIC_ACQUIRE); }
static inline struct llist_node *llist_del_first(struct llist_head *head)
{
    struct llist_node *e = READ_ONCE(head->first), *next;
    do {
        if (!e) return NULL;
        next = READ_ONCE(e->next);
    } while (!__atomic_compare_exchange_n(&head->first, &e, next, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED));
    return e;
}
static inline struct llist_node *llist_reverse_order(struct llist_node *head)
{
    struct llist_node *n = NULL;
    while (head) { struct llist_node *t = head; head = head->next; t->next = n; n = t; }
    return n;
}
#define llist_entry(ptr, type, member) container_of(ptr, type, member)
#define llist_for_each(pos, node) for ((pos) = (node); pos; (pos) = (pos)->next)
#define llist_for_each_safe(pos, n, node) for ((pos) = (node); (pos) && ((n) = (pos)->next, true); (pos) = (n))
#define llist_for_each_entry(pos, node, member) \
    for ((pos) = llist_entry((node), typeof(*(pos)), member); &(pos)->member != NULL; \
         (pos) = llist_entry((pos)->member.next, typeof(*(pos)), member))
#define llist_for_each_entry_safe(pos, n, node, member) \
    for (pos = llist_entry((node), typeof(*pos), member); &pos->member != NULL && \
         (n = llist_entry(pos->member.next, typeof(*n), member), true); pos = n)

#endif
