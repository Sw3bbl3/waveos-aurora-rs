/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: intrusive doubly linked lists (list_head) and hash lists
 * (hlist), with the interface Linux code uses.
 */
#ifndef _WAVEOS_LIST_H
#define _WAVEOS_LIST_H

#include <waveos/base.h>

struct list_head {
    struct list_head *next, *prev;
};
struct hlist_head {
    struct hlist_node *first;
};
struct hlist_node {
    struct hlist_node *next, **pprev;
};

#define LIST_HEAD_INIT(name) { &(name), &(name) }
#define LIST_HEAD(name) struct list_head name = LIST_HEAD_INIT(name)
#define LIST_POISON1 ((void *)0x100)
#define LIST_POISON2 ((void *)0x122)

static inline void INIT_LIST_HEAD(struct list_head *l) { WRITE_ONCE(l->next, l); l->prev = l; }
static inline void __list_add(struct list_head *n, struct list_head *prev, struct list_head *next)
{
    next->prev = n;
    n->next = next;
    n->prev = prev;
    WRITE_ONCE(prev->next, n);
}
static inline void list_add(struct list_head *n, struct list_head *head) { __list_add(n, head, head->next); }
static inline void list_add_tail(struct list_head *n, struct list_head *head) { __list_add(n, head->prev, head); }
static inline void __list_del(struct list_head *prev, struct list_head *next)
{
    next->prev = prev;
    WRITE_ONCE(prev->next, next);
}
static inline void __list_del_entry(struct list_head *e) { __list_del(e->prev, e->next); }
static inline void list_del(struct list_head *e)
{
    __list_del_entry(e);
    e->next = (struct list_head *)LIST_POISON1;
    e->prev = (struct list_head *)LIST_POISON2;
}
static inline void list_del_init(struct list_head *e) { __list_del_entry(e); INIT_LIST_HEAD(e); }
static inline void list_replace(struct list_head *old, struct list_head *n)
{
    n->next = old->next;
    n->next->prev = n;
    n->prev = old->prev;
    n->prev->next = n;
}
static inline void list_replace_init(struct list_head *old, struct list_head *n) { list_replace(old, n); INIT_LIST_HEAD(old); }
static inline void list_move(struct list_head *e, struct list_head *head) { __list_del_entry(e); list_add(e, head); }
static inline void list_move_tail(struct list_head *e, struct list_head *head) { __list_del_entry(e); list_add_tail(e, head); }
static inline bool list_is_first(const struct list_head *e, const struct list_head *head) { return e->prev == head; }
static inline bool list_is_last(const struct list_head *e, const struct list_head *head) { return e->next == head; }
static inline bool list_is_head(const struct list_head *e, const struct list_head *head) { return e == head; }
static inline bool list_empty(const struct list_head *head) { return READ_ONCE(head->next) == head; }
static inline bool list_empty_careful(const struct list_head *head) { return head->next == head && head->prev == head; }
static inline bool list_is_singular(const struct list_head *head) { return !list_empty(head) && head->next == head->prev; }
static inline size_t list_count_nodes(struct list_head *head)
{
    size_t n = 0;
    for (struct list_head *p = head->next; p != head; p = p->next) n++;
    return n;
}
static inline void __list_splice(const struct list_head *list, struct list_head *prev, struct list_head *next)
{
    struct list_head *first = list->next, *last = list->prev;
    first->prev = prev;
    prev->next = first;
    last->next = next;
    next->prev = last;
}
static inline void list_splice(const struct list_head *list, struct list_head *head)
{ if (!list_empty(list)) __list_splice(list, head, head->next); }
static inline void list_splice_tail(struct list_head *list, struct list_head *head)
{ if (!list_empty(list)) __list_splice(list, head->prev, head); }
static inline void list_splice_init(struct list_head *list, struct list_head *head)
{ if (!list_empty(list)) { __list_splice(list, head, head->next); INIT_LIST_HEAD(list); } }
static inline void list_splice_tail_init(struct list_head *list, struct list_head *head)
{ if (!list_empty(list)) { __list_splice(list, head->prev, head); INIT_LIST_HEAD(list); } }
static inline void list_rotate_left(struct list_head *head)
{ if (!list_empty(head)) list_move_tail(head->next, head); }
static inline void list_cut_position(struct list_head *list, struct list_head *head, struct list_head *entry)
{
    if (list_empty(head) || (list_is_singular(head) && head->next != entry && head != entry)) return;
    if (entry == head) { INIT_LIST_HEAD(list); return; }
    struct list_head *first = entry->next;
    list->next = head->next;
    list->next->prev = list;
    list->prev = entry;
    entry->next = list;
    head->next = first;
    first->prev = head;
}

#define list_entry(ptr, type, member) container_of(ptr, type, member)
#define list_first_entry(ptr, type, member) list_entry((ptr)->next, type, member)
#define list_last_entry(ptr, type, member) list_entry((ptr)->prev, type, member)
#define list_first_entry_or_null(ptr, type, member) \
    ({ struct list_head *__h = (ptr); struct list_head *__p = READ_ONCE(__h->next); __p != __h ? list_entry(__p, type, member) : NULL; })
#define list_last_entry_or_null(ptr, type, member) (list_empty(ptr) ? NULL : list_last_entry(ptr, type, member))
#define list_next_entry(pos, member) list_entry((pos)->member.next, typeof(*(pos)), member)
#define list_prev_entry(pos, member) list_entry((pos)->member.prev, typeof(*(pos)), member)
#define list_next_entry_circular(pos, head, member) \
    (list_is_last(&(pos)->member, head) ? list_first_entry(head, typeof(*(pos)), member) : list_next_entry(pos, member))
#define list_entry_is_head(pos, head, member) (&(pos)->member == (head))
#define list_for_each(pos, head) for (pos = (head)->next; pos != (head); pos = pos->next)
#define list_for_each_prev(pos, head) for (pos = (head)->prev; pos != (head); pos = pos->prev)
#define list_for_each_safe(pos, n, head) for (pos = (head)->next, n = pos->next; pos != (head); pos = n, n = pos->next)
#define list_for_each_prev_safe(pos, n, head) for (pos = (head)->prev, n = pos->prev; pos != (head); pos = n, n = pos->prev)
#define list_for_each_entry(pos, head, member) \
    for (pos = list_first_entry(head, typeof(*pos), member); !list_entry_is_head(pos, head, member); pos = list_next_entry(pos, member))
#define list_for_each_entry_reverse(pos, head, member) \
    for (pos = list_last_entry(head, typeof(*pos), member); !list_entry_is_head(pos, head, member); pos = list_prev_entry(pos, member))
#define list_for_each_entry_continue(pos, head, member) \
    for (pos = list_next_entry(pos, member); !list_entry_is_head(pos, head, member); pos = list_next_entry(pos, member))
#define list_for_each_entry_continue_reverse(pos, head, member) \
    for (pos = list_prev_entry(pos, member); !list_entry_is_head(pos, head, member); pos = list_prev_entry(pos, member))
#define list_for_each_entry_from(pos, head, member) for (; !list_entry_is_head(pos, head, member); pos = list_next_entry(pos, member))
#define list_for_each_entry_from_reverse(pos, head, member) for (; !list_entry_is_head(pos, head, member); pos = list_prev_entry(pos, member))
#define list_for_each_entry_safe(pos, n, head, member) \
    for (pos = list_first_entry(head, typeof(*pos), member), n = list_next_entry(pos, member); \
         !list_entry_is_head(pos, head, member); pos = n, n = list_next_entry(n, member))
#define list_for_each_entry_safe_reverse(pos, n, head, member) \
    for (pos = list_last_entry(head, typeof(*pos), member), n = list_prev_entry(pos, member); \
         !list_entry_is_head(pos, head, member); pos = n, n = list_prev_entry(n, member))
#define list_for_each_entry_safe_continue(pos, n, head, member) \
    for (pos = list_next_entry(pos, member), n = list_next_entry(pos, member); \
         !list_entry_is_head(pos, head, member); pos = n, n = list_next_entry(n, member))
#define list_safe_reset_next(pos, n, member) n = list_next_entry(pos, member)
#define list_prepare_entry(pos, head, member) ((pos) ? : list_entry(head, typeof(*pos), member))

/* RCU variants: WaveOS runs these lists under locks, so they are the plain ones. */
#define list_add_rcu list_add
#define list_add_tail_rcu list_add_tail
#define list_del_rcu list_del
#define list_for_each_entry_rcu(pos, head, member, ...) list_for_each_entry(pos, head, member)
#define list_first_or_null_rcu(ptr, type, member) list_first_entry_or_null(ptr, type, member)
#define list_entry_rcu list_entry

#define HLIST_HEAD_INIT { .first = NULL }
#define HLIST_HEAD(name) struct hlist_head name = { .first = NULL }
#define INIT_HLIST_HEAD(ptr) ((ptr)->first = NULL)
static inline void INIT_HLIST_NODE(struct hlist_node *h) { h->next = NULL; h->pprev = NULL; }
static inline bool hlist_unhashed(const struct hlist_node *h) { return !h->pprev; }
static inline bool hlist_empty(const struct hlist_head *h) { return !READ_ONCE(h->first); }
static inline void __hlist_del(struct hlist_node *n)
{
    struct hlist_node *next = n->next, **pprev = n->pprev;
    WRITE_ONCE(*pprev, next);
    if (next) next->pprev = pprev;
}
static inline void hlist_del(struct hlist_node *n)
{
    __hlist_del(n);
    n->next = (struct hlist_node *)LIST_POISON1;
    n->pprev = (struct hlist_node **)LIST_POISON2;
}
static inline void hlist_del_init(struct hlist_node *n) { if (!hlist_unhashed(n)) { __hlist_del(n); INIT_HLIST_NODE(n); } }
static inline void hlist_add_head(struct hlist_node *n, struct hlist_head *h)
{
    struct hlist_node *first = h->first;
    n->next = first;
    if (first) first->pprev = &n->next;
    WRITE_ONCE(h->first, n);
    n->pprev = &h->first;
}
#define hlist_add_head_rcu hlist_add_head
#define hlist_del_rcu hlist_del
#define hlist_del_init_rcu hlist_del_init
#define hlist_entry(ptr, type, member) container_of(ptr, type, member)
#define hlist_entry_safe(ptr, type, member) ({ typeof(ptr) ____p = (ptr); ____p ? hlist_entry(____p, type, member) : NULL; })
#define hlist_for_each(pos, head) for (pos = (head)->first; pos; pos = pos->next)
#define hlist_for_each_entry(pos, head, member) \
    for (pos = hlist_entry_safe((head)->first, typeof(*(pos)), member); pos; pos = hlist_entry_safe((pos)->member.next, typeof(*(pos)), member))
#define hlist_for_each_entry_rcu(pos, head, member, ...) hlist_for_each_entry(pos, head, member)
#define hlist_for_each_entry_safe(pos, n, head, member) \
    for (pos = hlist_entry_safe((head)->first, typeof(*pos), member); pos && ({ n = pos->member.next; 1; }); \
         pos = hlist_entry_safe(n, typeof(*pos), member))

/* Sorts a list stably by `cmp` (a merge sort; see lib.c). */
typedef int (*list_cmp_func_t)(void *priv, const struct list_head *a, const struct list_head *b);
void list_sort(void *priv, struct list_head *head, list_cmp_func_t cmp);

#endif
