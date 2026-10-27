// SPDX-License-Identifier: MIT
/*
 * WaveOS LinuxKPI: strings, character classes, number parsing, sorting and
 * bitmaps. memcpy, memmove, memset and memcmp come from Rust's
 * compiler-builtins, which the kernel already links.
 */
#include <waveos/kernel.h>

/* Bits: 1 upper, 2 lower, 4 digit, 8 control, 16 punctuation, 32 space, 64 hex digit, 128 hard space. */
const unsigned char _waveos_ctype[256] = {
    8, 8, 8, 8, 8, 8, 8, 8, 8, 40, 40, 40, 40, 40, 8, 8,
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
    160, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16,
    68, 68, 68, 68, 68, 68, 68, 68, 68, 68, 16, 16, 16, 16, 16, 16,
    16, 65, 65, 65, 65, 65, 65, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 16, 16, 16, 16, 16,
    16, 66, 66, 66, 66, 66, 66, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 16, 16, 16, 16, 8,
};
const char hex_asc[] = "0123456789abcdef";

void *memchr(const void *s, int c, size_t n)
{
    const unsigned char *p = s;
    for (; n; n--, p++)
        if (*p == (unsigned char)c)
            return (void *)p;
    return NULL;
}

void *memchr_inv(const void *s, int c, size_t n)
{
    const unsigned char *p = s;
    for (; n; n--, p++)
        if (*p != (unsigned char)c)
            return (void *)p;
    return NULL;
}

size_t strlen(const char *s)
{
    const char *p = s;
    while (*p)
        p++;
    return p - s;
}

size_t strnlen(const char *s, size_t max)
{
    size_t n = 0;
    while (n < max && s[n])
        n++;
    return n;
}

int strcmp(const char *a, const char *b)
{
    for (;; a++, b++) {
        if (*a != *b)
            return (unsigned char)*a < (unsigned char)*b ? -1 : 1;
        if (!*a)
            return 0;
    }
}

int strncmp(const char *a, const char *b, size_t n)
{
    for (; n; n--, a++, b++) {
        if (*a != *b)
            return (unsigned char)*a < (unsigned char)*b ? -1 : 1;
        if (!*a)
            return 0;
    }
    return 0;
}

int strcasecmp(const char *a, const char *b)
{
    for (;; a++, b++) {
        int x = tolower(*a), y = tolower(*b);
        if (x != y)
            return x < y ? -1 : 1;
        if (!x)
            return 0;
    }
}

int strncasecmp(const char *a, const char *b, size_t n)
{
    for (; n; n--, a++, b++) {
        int x = tolower(*a), y = tolower(*b);
        if (x != y)
            return x < y ? -1 : 1;
        if (!x)
            return 0;
    }
    return 0;
}

char *strcpy(char *dst, const char *src)
{
    char *d = dst;
    while ((*d++ = *src++))
        ;
    return dst;
}

char *strncpy(char *dst, const char *src, size_t n)
{
    size_t i = 0;
    for (; i < n && src[i]; i++)
        dst[i] = src[i];
    for (; i < n; i++)
        dst[i] = 0;
    return dst;
}

size_t strlcpy(char *dst, const char *src, size_t size)
{
    size_t len = strlen(src);
    if (size) {
        size_t n = len >= size ? size - 1 : len;
        memcpy(dst, src, n);
        dst[n] = 0;
    }
    return len;
}

ssize_t strscpy(char *dst, const char *src, size_t size)
{
    size_t i;
    if (!size)
        return -E2BIG;
    for (i = 0; i < size; i++) {
        dst[i] = src[i];
        if (!src[i])
            return i;
    }
    dst[size - 1] = 0;
    return -E2BIG;
}

ssize_t strscpy_pad(char *dst, const char *src, size_t size)
{
    ssize_t n = strscpy(dst, src, size);
    size_t used = n < 0 ? size : (size_t)n + 1;
    if (used < size)
        memset(dst + used, 0, size - used);
    return n;
}

size_t strlcat(char *dst, const char *src, size_t size)
{
    size_t d = strnlen(dst, size);
    if (d == size)
        return d + strlen(src);
    return d + strlcpy(dst + d, src, size - d);
}

char *strcat(char *dst, const char *src)
{
    strcpy(dst + strlen(dst), src);
    return dst;
}

char *strncat(char *dst, const char *src, size_t n)
{
    char *d = dst + strlen(dst);
    while (n-- && *src)
        *d++ = *src++;
    *d = 0;
    return dst;
}

char *strchr(const char *s, int c)
{
    for (;; s++) {
        if (*s == (char)c)
            return (char *)s;
        if (!*s)
            return NULL;
    }
}

char *strnchr(const char *s, size_t n, int c)
{
    for (; n && *s; n--, s++)
        if (*s == (char)c)
            return (char *)s;
    return NULL;
}

char *strrchr(const char *s, int c)
{
    const char *last = NULL;
    for (;; s++) {
        if (*s == (char)c)
            last = s;
        if (!*s)
            return (char *)last;
    }
}

char *strchrnul(const char *s, int c)
{
    while (*s && *s != (char)c)
        s++;
    return (char *)s;
}

char *strstr(const char *haystack, const char *needle)
{
    size_t n = strlen(needle);
    for (; *haystack; haystack++)
        if (!strncmp(haystack, needle, n))
            return (char *)haystack;
    return n ? NULL : (char *)haystack;
}

char *strnstr(const char *haystack, const char *needle, size_t len)
{
    size_t n = strlen(needle);
    for (; len >= n && *haystack; haystack++, len--)
        if (!memcmp(haystack, needle, n))
            return (char *)haystack;
    return NULL;
}

size_t strspn(const char *s, const char *accept)
{
    size_t n = 0;
    while (s[n] && strchr(accept, s[n]))
        n++;
    return n;
}

size_t strcspn(const char *s, const char *reject)
{
    size_t n = 0;
    while (s[n] && !strchr(reject, s[n]))
        n++;
    return n;
}

char *strpbrk(const char *s, const char *accept)
{
    s += strcspn(s, accept);
    return *s ? (char *)s : NULL;
}

char *strsep(char **s, const char *delim)
{
    char *start = *s, *end;
    if (!start)
        return NULL;
    end = strpbrk(start, delim);
    if (end)
        *end++ = 0;
    *s = end;
    return start;
}

char *skip_spaces(const char *s)
{
    while (isspace(*s))
        s++;
    return (char *)s;
}

char *strim(char *s)
{
    size_t n = strlen(s);
    while (n && isspace(s[n - 1]))
        s[--n] = 0;
    return skip_spaces(s);
}

bool sysfs_streq(const char *a, const char *b)
{
    while (*a && *a == *b) {
        a++;
        b++;
    }
    if (*a == *b)
        return true;
    if (!*a && *b == '\n' && !b[1])
        return true;
    if (*a == '\n' && !a[1] && !*b)
        return true;
    return false;
}

int match_string(const char *const *array, size_t n, const char *string)
{
    for (size_t i = 0; i < n; i++) {
        if (!array[i])
            break;
        if (!strcmp(array[i], string))
            return i;
    }
    return -EINVAL;
}

/* ---- Allocated strings ---- */
char *kstrdup(const char *s, gfp_t gfp)
{
    size_t n;
    char *d;
    if (!s)
        return NULL;
    n = strlen(s) + 1;
    d = kmalloc(n, gfp);
    if (d)
        memcpy(d, s, n);
    return d;
}

const char *kstrdup_const(const char *s, gfp_t gfp) { return kstrdup(s, gfp); }

char *kstrndup(const char *s, size_t max, gfp_t gfp)
{
    size_t n;
    char *d;
    if (!s)
        return NULL;
    n = strnlen(s, max);
    d = kmalloc(n + 1, gfp);
    if (d) {
        memcpy(d, s, n);
        d[n] = 0;
    }
    return d;
}

char *kmemdup_nul(const char *s, size_t len, gfp_t gfp)
{
    char *d = kmalloc(len + 1, gfp);
    if (d) {
        memcpy(d, s, len);
        d[len] = 0;
    }
    return d;
}

void *memdup_user(const void __user *src, size_t len)
{
    void *d = kmalloc(len, GFP_KERNEL);
    if (!d)
        return ERR_PTR(-ENOMEM);
    memcpy(d, (const void *)src, len);
    return d;
}

void *vmemdup_user(const void __user *src, size_t len) { return memdup_user(src, len); }

/* ---- Numbers ---- */
static unsigned int digit_value(char c)
{
    if (c >= '0' && c <= '9')
        return c - '0';
    c = tolower(c);
    if (c >= 'a' && c <= 'z')
        return c - 'a' + 10;
    return 99;
}

/* Detects 0x / 0 prefixes for base 0 (and skips 0x for base 16). */
static const char *parse_base(const char *s, unsigned int *base)
{
    if (*base == 0) {
        if (s[0] == '0' && tolower(s[1]) == 'x' && isxdigit(s[2])) {
            *base = 16;
            s += 2;
        } else if (s[0] == '0') {
            *base = 8;
        } else {
            *base = 10;
        }
    } else if (*base == 16 && s[0] == '0' && tolower(s[1]) == 'x') {
        s += 2;
    }
    return s;
}

unsigned long long simple_strtoull(const char *cp, char **endp, unsigned int base)
{
    unsigned long long v = 0;
    unsigned int d;
    cp = parse_base(cp, &base);
    while ((d = digit_value(*cp)) < base) {
        v = v * base + d;
        cp++;
    }
    if (endp)
        *endp = (char *)cp;
    return v;
}

unsigned long simple_strtoul(const char *cp, char **endp, unsigned int base) { return simple_strtoull(cp, endp, base); }

long long simple_strtoll(const char *cp, char **endp, unsigned int base)
{
    if (*cp == '-')
        return -(long long)simple_strtoull(cp + 1, endp, base);
    return simple_strtoull(cp, endp, base);
}

long simple_strtol(const char *cp, char **endp, unsigned int base) { return simple_strtoll(cp, endp, base); }

int kstrtoull(const char *s, unsigned int base, unsigned long long *res)
{
    unsigned long long v = 0;
    unsigned int d;
    const char *start;
    if (*s == '+')
        s++;
    s = parse_base(s, &base);
    start = s;
    while ((d = digit_value(*s)) < base) {
        if (v > (ULLONG_MAX - d) / base)
            return -ERANGE;
        v = v * base + d;
        s++;
    }
    if (s == start)
        return -EINVAL;
    if (*s == '\n')
        s++;
    if (*s)
        return -EINVAL;
    *res = v;
    return 0;
}

int kstrtoll(const char *s, unsigned int base, long long *res)
{
    unsigned long long v;
    int r;
    if (*s == '-') {
        r = kstrtoull(s + 1, base, &v);
        if (r)
            return r;
        if (v > (unsigned long long)LLONG_MAX + 1)
            return -ERANGE;
        *res = -(long long)v;
        return 0;
    }
    r = kstrtoull(s, base, &v);
    if (r)
        return r;
    if (v > LLONG_MAX)
        return -ERANGE;
    *res = v;
    return 0;
}

int kstrtoul(const char *s, unsigned int base, unsigned long *res)
{
    unsigned long long v;
    int r = kstrtoull(s, base, &v);
    if (!r)
        *res = v;
    return r;
}

int kstrtol(const char *s, unsigned int base, long *res)
{
    long long v;
    int r = kstrtoll(s, base, &v);
    if (!r)
        *res = v;
    return r;
}

int kstrtouint(const char *s, unsigned int base, unsigned int *res)
{
    unsigned long long v;
    int r = kstrtoull(s, base, &v);
    if (r)
        return r;
    if (v > UINT_MAX)
        return -ERANGE;
    *res = v;
    return 0;
}

int kstrtoint(const char *s, unsigned int base, int *res)
{
    long long v;
    int r = kstrtoll(s, base, &v);
    if (r)
        return r;
    if (v < INT_MIN || v > INT_MAX)
        return -ERANGE;
    *res = v;
    return 0;
}

int kstrtobool(const char *s, bool *res)
{
    if (!s)
        return -EINVAL;
    switch (tolower(s[0])) {
    case 'y':
    case 't':
    case '1':
        *res = true;
        return 0;
    case 'n':
    case 'f':
    case '0':
        *res = false;
        return 0;
    case 'o':
        if (tolower(s[1]) == 'n') {
            *res = true;
            return 0;
        }
        if (tolower(s[1]) == 'f') {
            *res = false;
            return 0;
        }
        break;
    }
    return -EINVAL;
}

unsigned long int_sqrt(unsigned long x)
{
    unsigned long r = 0, bit = 1UL << (BITS_PER_LONG - 2);
    while (bit > x)
        bit >>= 2;
    while (bit) {
        if (x >= r + bit) {
            x -= r + bit;
            r = (r >> 1) + bit;
        } else {
            r >>= 1;
        }
        bit >>= 2;
    }
    return r;
}

u32 int_sqrt64(u64 x) { return int_sqrt(x); }

unsigned long gcd(unsigned long a, unsigned long b)
{
    while (b) {
        unsigned long t = a % b;
        a = b;
        b = t;
    }
    return a;
}

unsigned long lcm(unsigned long a, unsigned long b) { return a && b ? (a / gcd(a, b)) * b : 0; }

/* ---- Bitmaps ---- */
unsigned long find_next_bit(const unsigned long *addr, unsigned long size, unsigned long offset)
{
    for (; offset < size; offset++)
        if (addr[offset / BITS_PER_LONG] & (1UL << (offset % BITS_PER_LONG)))
            return offset;
    return size;
}

unsigned long find_next_zero_bit(const unsigned long *addr, unsigned long size, unsigned long offset)
{
    for (; offset < size; offset++)
        if (!(addr[offset / BITS_PER_LONG] & (1UL << (offset % BITS_PER_LONG))))
            return offset;
    return size;
}

/* ---- Sorting ---- */
static void swap_bytes(void *a, void *b, size_t size)
{
    unsigned char *x = a, *y = b;
    while (size--) {
        unsigned char t = *x;
        *x++ = *y;
        *y++ = t;
    }
}

/* Heapsort: no recursion, no allocation, O(n log n). */
void sort_r(void *base, size_t num, size_t size, int (*cmp)(const void *, const void *, const void *),
            void (*swap_fn)(void *, void *, int), const void *priv)
{
    char *b = base;
    size_t i, n;
    if (num < 2)
        return;
#define ELEM(k) (b + (k) * size)
#define SWAP(x, y) (swap_fn ? swap_fn(ELEM(x), ELEM(y), size) : swap_bytes(ELEM(x), ELEM(y), size))
    for (i = num / 2; i-- > 0;) {
        size_t root = i, child;
        while ((child = 2 * root + 1) < num) {
            if (child + 1 < num && cmp(ELEM(child), ELEM(child + 1), priv) < 0)
                child++;
            if (cmp(ELEM(root), ELEM(child), priv) >= 0)
                break;
            SWAP(root, child);
            root = child;
        }
    }
    for (n = num - 1; n > 0; n--) {
        size_t root = 0, child;
        SWAP(0, n);
        while ((child = 2 * root + 1) < n) {
            if (child + 1 < n && cmp(ELEM(child), ELEM(child + 1), priv) < 0)
                child++;
            if (cmp(ELEM(root), ELEM(child), priv) >= 0)
                break;
            SWAP(root, child);
            root = child;
        }
    }
#undef SWAP
#undef ELEM
}

static int cmp_unwrapped(const void *a, const void *b, const void *priv)
{
    return ((int (*)(const void *, const void *))priv)(a, b);
}

void sort(void *base, size_t num, size_t size, int (*cmp)(const void *, const void *), void (*swap_fn)(void *, void *, int))
{
    sort_r(base, num, size, cmp_unwrapped, swap_fn, (const void *)cmp);
}

/* Stable merge sort of a list (insertion into sorted runs, then merging). */
static struct list_head *merge(void *priv, list_cmp_func_t cmp, struct list_head *a, struct list_head *b)
{
    struct list_head *head = NULL, **tail = &head;
    while (a && b) {
        if (cmp(priv, a, b) <= 0) {
            *tail = a;
            a = a->next;
        } else {
            *tail = b;
            b = b->next;
        }
        tail = &(*tail)->next;
    }
    *tail = a ? a : b;
    return head;
}

void list_sort(void *priv, struct list_head *head, list_cmp_func_t cmp)
{
    struct list_head *bins[64] = { NULL };
    struct list_head *list, *result = NULL, *prev;
    int i, max = 0;
    if (list_empty(head) || head->next == head->prev)
        return;
    /* Break the circular list into a NULL-terminated singly linked one. */
    head->prev->next = NULL;
    list = head->next;
    while (list) {
        struct list_head *item = list;
        list = list->next;
        item->next = NULL;
        for (i = 0; i < 64 && bins[i]; i++) {
            item = merge(priv, cmp, bins[i], item);
            bins[i] = NULL;
        }
        if (i == 64)
            i = 63;
        bins[i] = item;
        if (i > max)
            max = i;
    }
    for (i = 0; i <= max; i++)
        if (bins[i])
            result = result ? merge(priv, cmp, bins[i], result) : bins[i];
    /* Rebuild the prev links and close the circle. */
    prev = head;
    head->next = result;
    for (list = result; list; list = list->next) {
        list->prev = prev;
        prev = list;
    }
    prev->next = head;
    head->prev = prev;
}
