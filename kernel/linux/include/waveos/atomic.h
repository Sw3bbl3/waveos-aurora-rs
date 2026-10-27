/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: atomics, memory barriers, bit operations and 64-bit
 * division helpers, on the compiler's builtins.
 */
#ifndef _WAVEOS_ATOMIC_H
#define _WAVEOS_ATOMIC_H

#include <waveos/base.h>

#define mb() __atomic_thread_fence(__ATOMIC_SEQ_CST)
#define rmb() __atomic_thread_fence(__ATOMIC_ACQUIRE)
#define wmb() __atomic_thread_fence(__ATOMIC_RELEASE)
#define smp_mb() mb()
#define smp_rmb() rmb()
#define smp_wmb() wmb()
#define dma_mb() mb()
#define dma_rmb() rmb()
#define dma_wmb() wmb()
#define virt_mb() mb()
#define virt_rmb() rmb()
#define virt_wmb() wmb()
#define virt_store_mb(var, value) do { WRITE_ONCE(var, value); mb(); } while (0)
#define smp_mb__before_atomic() mb()
#define smp_mb__after_atomic() mb()
#define smp_store_mb(var, value) do { WRITE_ONCE(var, value); mb(); } while (0)
#define smp_load_acquire(p) __atomic_load_n(p, __ATOMIC_ACQUIRE)
#define smp_store_release(p, v) __atomic_store_n(p, v, __ATOMIC_RELEASE)
#define cpu_relax() __builtin_ia32_pause()

#define ATOMIC_INIT(i) { (i) }
#define ATOMIC64_INIT(i) { (i) }
#define ATOMIC_LONG_INIT(i) { (i) }

#define WAVEOS_ATOMIC_OPS(prefix, atype, vtype)                                                              \
    static inline vtype prefix##_read(const atype *v) { return __atomic_load_n(&v->counter, __ATOMIC_RELAXED); } \
    static inline void prefix##_set(atype *v, vtype i) { __atomic_store_n(&v->counter, i, __ATOMIC_RELAXED); }   \
    static inline vtype prefix##_read_acquire(const atype *v) { return __atomic_load_n(&v->counter, __ATOMIC_ACQUIRE); } \
    static inline void prefix##_set_release(atype *v, vtype i) { __atomic_store_n(&v->counter, i, __ATOMIC_RELEASE); } \
    static inline void prefix##_add(vtype i, atype *v) { __atomic_fetch_add(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline void prefix##_sub(vtype i, atype *v) { __atomic_fetch_sub(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline void prefix##_inc(atype *v) { prefix##_add(1, v); }                                              \
    static inline void prefix##_dec(atype *v) { prefix##_sub(1, v); }                                              \
    static inline vtype prefix##_add_return(vtype i, atype *v) { return __atomic_add_fetch(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline vtype prefix##_sub_return(vtype i, atype *v) { return __atomic_sub_fetch(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline vtype prefix##_fetch_add(vtype i, atype *v) { return __atomic_fetch_add(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline vtype prefix##_fetch_sub(vtype i, atype *v) { return __atomic_fetch_sub(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline vtype prefix##_fetch_or(vtype i, atype *v) { return __atomic_fetch_or(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline vtype prefix##_fetch_and(vtype i, atype *v) { return __atomic_fetch_and(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline void prefix##_or(vtype i, atype *v) { __atomic_fetch_or(&v->counter, i, __ATOMIC_SEQ_CST); }    \
    static inline void prefix##_and(vtype i, atype *v) { __atomic_fetch_and(&v->counter, i, __ATOMIC_SEQ_CST); }  \
    static inline void prefix##_andnot(vtype i, atype *v) { __atomic_fetch_and(&v->counter, ~i, __ATOMIC_SEQ_CST); } \
    static inline vtype prefix##_inc_return(atype *v) { return prefix##_add_return(1, v); }                        \
    static inline vtype prefix##_dec_return(atype *v) { return prefix##_sub_return(1, v); }                        \
    static inline vtype prefix##_fetch_inc(atype *v) { return prefix##_fetch_add(1, v); }                          \
    static inline bool prefix##_dec_and_test(atype *v) { return prefix##_sub_return(1, v) == 0; }                 \
    static inline bool prefix##_inc_and_test(atype *v) { return prefix##_add_return(1, v) == 0; }                 \
    static inline bool prefix##_sub_and_test(vtype i, atype *v) { return prefix##_sub_return(i, v) == 0; }        \
    static inline vtype prefix##_xchg(atype *v, vtype n) { return __atomic_exchange_n(&v->counter, n, __ATOMIC_SEQ_CST); } \
    static inline vtype prefix##_cmpxchg(atype *v, vtype o, vtype n)                                                \
    { __atomic_compare_exchange_n(&v->counter, &o, n, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); return o; }      \
    static inline bool prefix##_try_cmpxchg(atype *v, vtype *o, vtype n)                                           \
    { return __atomic_compare_exchange_n(&v->counter, o, n, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); }          \
    static inline bool prefix##_add_unless(atype *v, vtype a, vtype u)                                             \
    {                                                                                                              \
        vtype c = prefix##_read(v);                                                                                \
        do { if (c == u) return false; } while (!prefix##_try_cmpxchg(v, &c, c + a));                              \
        return true;                                                                                               \
    }                                                                                                              \
    static inline bool prefix##_inc_not_zero(atype *v) { return prefix##_add_unless(v, 1, 0); }

WAVEOS_ATOMIC_OPS(atomic, atomic_t, int)
WAVEOS_ATOMIC_OPS(atomic64, atomic64_t, s64)
#define atomic_long_read atomic64_read
#define atomic_long_set atomic64_set
#define atomic_long_inc atomic64_inc
#define atomic_long_dec atomic64_dec
#define atomic_long_add atomic64_add
#define atomic_long_sub atomic64_sub
#define atomic_long_add_return atomic64_add_return
#define atomic_long_inc_return atomic64_inc_return
#define atomic_long_dec_return atomic64_dec_return
#define atomic_long_cmpxchg atomic64_cmpxchg
#define atomic_long_xchg atomic64_xchg

#define xchg(ptr, v) __atomic_exchange_n(ptr, v, __ATOMIC_SEQ_CST)
#define cmpxchg(ptr, o, n) ({ typeof(*(ptr)) __o = (o); __atomic_compare_exchange_n(ptr, &__o, n, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); __o; })
#define try_cmpxchg(ptr, po, n) __atomic_compare_exchange_n(ptr, po, n, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)
#define cmpxchg64 cmpxchg
#define cmpxchg_relaxed cmpxchg

/* ---- refcount_t ---- */
#define REFCOUNT_INIT(n) { .refs = ATOMIC_INIT(n) }
static inline void refcount_set(refcount_t *r, int n) { atomic_set(&r->refs, n); }
static inline unsigned int refcount_read(const refcount_t *r) { return atomic_read(&r->refs); }
static inline void refcount_inc(refcount_t *r) { atomic_inc(&r->refs); }
static inline void refcount_add(int i, refcount_t *r) { atomic_add(i, &r->refs); }
static inline bool refcount_inc_not_zero(refcount_t *r) { return atomic_inc_not_zero(&r->refs); }
static inline bool refcount_dec_and_test(refcount_t *r) { return atomic_dec_and_test(&r->refs); }
static inline bool refcount_sub_and_test(int i, refcount_t *r) { return atomic_sub_and_test(i, &r->refs); }
static inline void refcount_dec(refcount_t *r) { atomic_dec(&r->refs); }

/* ---- Bit operations on unsigned long arrays ---- */
static inline void set_bit(long nr, volatile unsigned long *addr)
{ __atomic_fetch_or(&addr[BIT_WORD(nr)], BIT_MASK(nr), __ATOMIC_SEQ_CST); }
static inline void clear_bit(long nr, volatile unsigned long *addr)
{ __atomic_fetch_and(&addr[BIT_WORD(nr)], ~BIT_MASK(nr), __ATOMIC_SEQ_CST); }
static inline void change_bit(long nr, volatile unsigned long *addr)
{ __atomic_fetch_xor(&addr[BIT_WORD(nr)], BIT_MASK(nr), __ATOMIC_SEQ_CST); }
static inline bool test_bit(long nr, const volatile unsigned long *addr)
{ return (__atomic_load_n(&addr[BIT_WORD(nr)], __ATOMIC_RELAXED) & BIT_MASK(nr)) != 0; }
static inline bool test_and_set_bit(long nr, volatile unsigned long *addr)
{ return (__atomic_fetch_or(&addr[BIT_WORD(nr)], BIT_MASK(nr), __ATOMIC_SEQ_CST) & BIT_MASK(nr)) != 0; }
static inline bool test_and_clear_bit(long nr, volatile unsigned long *addr)
{ return (__atomic_fetch_and(&addr[BIT_WORD(nr)], ~BIT_MASK(nr), __ATOMIC_SEQ_CST) & BIT_MASK(nr)) != 0; }
#define __set_bit(nr, addr) set_bit(nr, addr)
#define __clear_bit(nr, addr) clear_bit(nr, addr)
#define __test_and_set_bit(nr, addr) test_and_set_bit(nr, addr)
#define __test_and_clear_bit(nr, addr) test_and_clear_bit(nr, addr)
#define clear_bit_unlock(nr, addr) clear_bit(nr, addr)
#define test_and_set_bit_lock(nr, addr) test_and_set_bit(nr, addr)

static inline unsigned long __ffs(unsigned long w) { return __builtin_ctzl(w); }
static inline unsigned long __fls(unsigned long w) { return 63 - __builtin_clzl(w); }
static inline unsigned long ffz(unsigned long w) { return __builtin_ctzl(~w); }
static inline int ffs(int x) { return __builtin_ffs(x); }
static inline int fls(unsigned int x) { return x ? 32 - __builtin_clz(x) : 0; }
static inline int fls64(u64 x) { return x ? 64 - __builtin_clzll(x) : 0; }
static inline int fls_long(unsigned long x) { return fls64(x); }
static inline unsigned int hweight8(unsigned int w) { return __builtin_popcount(w & 0xff); }
static inline unsigned int hweight16(unsigned int w) { return __builtin_popcount(w & 0xffff); }
static inline unsigned int hweight32(unsigned int w) { return __builtin_popcount(w); }
static inline unsigned long hweight64(u64 w) { return __builtin_popcountll(w); }
static inline unsigned long hweight_long(unsigned long w) { return __builtin_popcountl(w); }
static inline u32 ror32(u32 w, unsigned int s) { return (w >> (s & 31)) | (w << ((-s) & 31)); }
static inline u32 rol32(u32 w, unsigned int s) { return (w << (s & 31)) | (w >> ((-s) & 31)); }
static inline unsigned long __roundup_pow_of_two(unsigned long n) { return n <= 1 ? 1 : 1UL << fls_long(n - 1); }
static inline unsigned long __rounddown_pow_of_two(unsigned long n) { return 1UL << (fls_long(n) - 1); }
#define roundup_pow_of_two(n) __roundup_pow_of_two(n)
#define rounddown_pow_of_two(n) __rounddown_pow_of_two(n)
static inline bool is_power_of_2(unsigned long n) { return n != 0 && (n & (n - 1)) == 0; }
static inline int ilog2(u64 n) { return fls64(n) - 1; }
#define order_base_2(n) ((n) > 1 ? ilog2((n) - 1) + 1 : 0)
static inline int get_count_order(unsigned int count) { return count == 0 ? -1 : fls(count - 1); }
#define for_each_set_bit(bit, addr, size) \
    for ((bit) = find_first_bit((addr), (size)); (bit) < (size); (bit) = find_next_bit((addr), (size), (bit) + 1))
#define for_each_clear_bit(bit, addr, size) \
    for ((bit) = find_first_zero_bit((addr), (size)); (bit) < (size); (bit) = find_next_zero_bit((addr), (size), (bit) + 1))
unsigned long find_next_bit(const unsigned long *addr, unsigned long size, unsigned long offset);
unsigned long find_next_zero_bit(const unsigned long *addr, unsigned long size, unsigned long offset);
static inline unsigned long find_first_bit(const unsigned long *addr, unsigned long size) { return find_next_bit(addr, size, 0); }
static inline unsigned long find_first_zero_bit(const unsigned long *addr, unsigned long size)
{ return find_next_zero_bit(addr, size, 0); }
static inline unsigned long find_last_bit(const unsigned long *addr, unsigned long size)
{
    for (unsigned long i = size; i-- > 0;)
        if (test_bit(i, addr)) return i;
    return size;
}

/* ---- Bitmaps ---- */
#define DECLARE_BITMAP(name, bits) unsigned long name[BITS_TO_LONGS(bits)]
static inline void bitmap_zero(unsigned long *dst, unsigned int nbits) { __builtin_memset(dst, 0, BITS_TO_LONGS(nbits) * sizeof(long)); }
static inline void bitmap_fill(unsigned long *dst, unsigned int nbits) { __builtin_memset(dst, 0xff, BITS_TO_LONGS(nbits) * sizeof(long)); }
static inline bool bitmap_empty(const unsigned long *src, unsigned int nbits) { return find_first_bit(src, nbits) >= nbits; }
static inline bool bitmap_full(const unsigned long *src, unsigned int nbits) { return find_first_zero_bit(src, nbits) >= nbits; }
static inline unsigned int bitmap_weight(const unsigned long *src, unsigned int nbits)
{ unsigned int w = 0; for (unsigned int i = 0; i < nbits; i++) w += test_bit(i, src); return w; }
static inline void bitmap_set(unsigned long *map, unsigned int start, unsigned int len)
{ for (unsigned int i = start; i < start + len; i++) set_bit(i, map); }
static inline void bitmap_clear(unsigned long *map, unsigned int start, unsigned int len)
{ for (unsigned int i = start; i < start + len; i++) clear_bit(i, map); }
static inline void bitmap_copy(unsigned long *dst, const unsigned long *src, unsigned int nbits)
{ __builtin_memcpy(dst, src, BITS_TO_LONGS(nbits) * sizeof(long)); }
static inline void bitmap_or(unsigned long *dst, const unsigned long *a, const unsigned long *b, unsigned int nbits)
{ for (unsigned int i = 0; i < BITS_TO_LONGS(nbits); i++) dst[i] = a[i] | b[i]; }
static inline bool bitmap_and(unsigned long *dst, const unsigned long *a, const unsigned long *b, unsigned int nbits)
{ unsigned long r = 0; for (unsigned int i = 0; i < BITS_TO_LONGS(nbits); i++) r |= (dst[i] = a[i] & b[i]); return r != 0; }
static inline bool bitmap_equal(const unsigned long *a, const unsigned long *b, unsigned int nbits)
{ for (unsigned int i = 0; i < nbits; i++) if (test_bit(i, a) != test_bit(i, b)) return false; return true; }
unsigned long bitmap_find_next_zero_area(unsigned long *map, unsigned long size, unsigned long start, unsigned int nr,
                                         unsigned long align_mask);

/* ---- 64-bit division ---- */
static inline u64 div_u64_rem(u64 dividend, u32 divisor, u32 *remainder)
{ *remainder = dividend % divisor; return dividend / divisor; }
static inline u64 div_u64(u64 dividend, u32 divisor) { return dividend / divisor; }
static inline s64 div_s64(s64 dividend, s32 divisor) { return dividend / divisor; }
static inline s64 div_s64_rem(s64 dividend, s32 divisor, s32 *remainder)
{ *remainder = dividend % divisor; return dividend / divisor; }
static inline u64 div64_u64(u64 dividend, u64 divisor) { return dividend / divisor; }
static inline u64 div64_u64_rem(u64 dividend, u64 divisor, u64 *remainder)
{ *remainder = dividend % divisor; return dividend / divisor; }
static inline s64 div64_s64(s64 dividend, s64 divisor) { return dividend / divisor; }
static inline u64 div64_ul(u64 dividend, unsigned long divisor) { return dividend / divisor; }
#define div64_long(x, y) div64_s64((x), (y))
#define DIV64_U64_ROUND_UP(ll, d) ({ u64 _tmp = (d); div64_u64((ll) + _tmp - 1, _tmp); })
#define DIV64_U64_ROUND_CLOSEST(dividend, divisor) ({ u64 _tmp = (divisor); div64_u64((dividend) + _tmp / 2, _tmp); })
#define do_div(n, base) ({ u32 __base = (base); u32 __rem = (u64)(n) % __base; (n) = (u64)(n) / __base; __rem; })
static inline u64 mul_u32_u32(u32 a, u32 b) { return (u64)a * b; }
static inline u64 mul_u64_u32_div(u64 a, u32 mul, u32 divisor)
{ return (u64)(((unsigned __int128)a * mul) / divisor); }
static inline u64 mul_u64_u64_div_u64(u64 a, u64 mul, u64 div) { return (u64)(((unsigned __int128)a * mul) / div); }
static inline u64 mul_u64_u32_shr(u64 a, u32 mul, unsigned int shift) { return (u64)(((unsigned __int128)a * mul) >> shift); }
static inline u32 reciprocal_scale(u32 val, u32 ep_ro) { return (u32)(((u64)val * ep_ro) >> 32); }
unsigned long int_sqrt(unsigned long x);
u32 int_sqrt64(u64 x);
unsigned long gcd(unsigned long a, unsigned long b);
unsigned long lcm(unsigned long a, unsigned long b);
static inline u32 get_unaligned_le32(const void *p) { u32 v; __builtin_memcpy(&v, p, 4); return v; }
static inline u16 get_unaligned_le16(const void *p) { u16 v; __builtin_memcpy(&v, p, 2); return v; }
static inline u64 get_unaligned_le64(const void *p) { u64 v; __builtin_memcpy(&v, p, 8); return v; }
static inline void put_unaligned_le32(u32 v, void *p) { __builtin_memcpy(p, &v, 4); }
static inline void put_unaligned_le16(u16 v, void *p) { __builtin_memcpy(p, &v, 2); }
static inline u32 get_unaligned_be32(const void *p) { return __builtin_bswap32(get_unaligned_le32(p)); }
static inline u16 get_unaligned_be16(const void *p) { return __builtin_bswap16(get_unaligned_le16(p)); }
static inline void put_unaligned_be32(u32 v, void *p) { put_unaligned_le32(__builtin_bswap32(v), p); }
static inline void put_unaligned_be16(u16 v, void *p) { put_unaligned_le16(__builtin_bswap16(v), p); }
#define get_unaligned(p) ({ typeof(*(p)) __v; __builtin_memcpy(&__v, (p), sizeof(__v)); __v; })
#define put_unaligned(v, p) do { typeof(*(p)) __v = (v); __builtin_memcpy((p), &__v, sizeof(__v)); } while (0)

/* Little-endian x86: CPU order is little endian. */
#define cpu_to_le16(x) ((__le16)(u16)(x))
#define cpu_to_le32(x) ((__le32)(u32)(x))
#define cpu_to_le64(x) ((__le64)(u64)(x))
#define le16_to_cpu(x) ((u16)(__le16)(x))
#define le32_to_cpu(x) ((u32)(__le32)(x))
#define le64_to_cpu(x) ((u64)(__le64)(x))
#define cpu_to_be16(x) ((__be16)__builtin_bswap16(x))
#define cpu_to_be32(x) ((__be32)__builtin_bswap32(x))
#define cpu_to_be64(x) ((__be64)__builtin_bswap64(x))
#define be16_to_cpu(x) __builtin_bswap16((u16)(x))
#define be32_to_cpu(x) __builtin_bswap32((u32)(x))
#define be64_to_cpu(x) __builtin_bswap64((u64)(x))
#define le16_to_cpup(p) le16_to_cpu(*(p))
#define le32_to_cpup(p) le32_to_cpu(*(p))
#define le32_add_cpu(p, v) (*(p) = cpu_to_le32(le32_to_cpu(*(p)) + (v)))
#define swab16 __builtin_bswap16
#define swab32 __builtin_bswap32
#define swab64 __builtin_bswap64
#define __cpu_to_le32 cpu_to_le32
#define __le32_to_cpu le32_to_cpu
#define __cpu_to_le16 cpu_to_le16
#define __le16_to_cpu le16_to_cpu
#define __cpu_to_le64 cpu_to_le64
#define __le64_to_cpu le64_to_cpu
#define htonl(x) cpu_to_be32(x)
#define ntohl(x) be32_to_cpu(x)
#define htons(x) cpu_to_be16(x)
#define ntohs(x) be16_to_cpu(x)

#endif
