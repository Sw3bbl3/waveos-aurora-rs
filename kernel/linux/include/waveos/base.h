/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: the base of the Linux kernel API, written for WaveOS.
 *
 * Types, compiler attributes, the common macros (container_of, min, max,
 * BIT, ...), error pointers and READ_ONCE/WRITE_ONCE. Every other LinuxKPI
 * header builds on this one. These are independent implementations of the
 * interfaces Linux drivers expect; no Linux header text is used.
 */
#ifndef _WAVEOS_BASE_H
#define _WAVEOS_BASE_H

#include <stdarg.h>
#include <stddef.h>

/* ---- Fixed-width types ------------------------------------------------- */

typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;
typedef signed char s8;
typedef signed short s16;
typedef signed int s32;
typedef signed long long s64;
/* As in Linux, the C99 names are the kernel types (uint64_t is unsigned long long). */
typedef u8 uint8_t;
typedef u16 uint16_t;
typedef u32 uint32_t;
typedef u64 uint64_t;
typedef s8 int8_t;
typedef s16 int16_t;
typedef s32 int32_t;
typedef s64 int64_t;
typedef unsigned long uintptr_t;
typedef long intptr_t;
typedef u8 __u8;
typedef u16 __u16;
typedef u32 __u32;
typedef u64 __u64;
typedef s8 __s8;
typedef s16 __s16;
typedef s32 __s32;
typedef s64 __s64;
typedef u16 __le16;
typedef u32 __le32;
typedef u64 __le64;
typedef u16 __be16;
typedef u32 __be32;
typedef u64 __be64;
typedef u16 __sum16;
typedef u32 __wsum;
typedef unsigned long __kernel_ulong_t;
typedef long __kernel_long_t;
typedef unsigned long __kernel_size_t;
typedef long __kernel_ssize_t;
typedef long __kernel_off_t;
typedef long long __kernel_loff_t;
typedef int __kernel_pid_t;
typedef unsigned int __kernel_uid32_t;
typedef unsigned int __kernel_gid32_t;
typedef long __kernel_time64_t_unused;

#ifndef __cplusplus
typedef _Bool bool;
#define true 1
#define false 0
#endif

typedef long ssize_t;
typedef long long loff_t;
typedef unsigned long pgoff_t;
typedef u64 phys_addr_t;
typedef u64 dma_addr_t;
typedef unsigned int uint;
typedef unsigned short ushort;
typedef unsigned long ulong;
typedef unsigned char uchar;
typedef u64 resource_size_t;
typedef unsigned int gfp_t;
typedef unsigned int fmode_t;
typedef unsigned short umode_t;
typedef int pid_t;
typedef unsigned int uid_t;
typedef unsigned int gid_t;
typedef struct { uid_t val; } kuid_t;
typedef struct { gid_t val; } kgid_t;
typedef s64 time64_t;
typedef u64 sector_t;
typedef u32 slab_flags_t;
typedef unsigned long irq_hw_number_t;
typedef int irqreturn_t_int;
typedef struct { int counter; } atomic_t;
typedef struct { s64 counter; } atomic64_t;
typedef atomic64_t atomic_long_t;
typedef struct { atomic_t refs; } refcount_t;

/* ---- Compiler attributes ----------------------------------------------- */

#define __always_inline inline __attribute__((__always_inline__))
#define noinline __attribute__((__noinline__))
#define __noreturn __attribute__((__noreturn__))
#define __packed __attribute__((__packed__))
#define __aligned(x) __attribute__((__aligned__(x)))
#define __printf(a, b) __attribute__((__format__(printf, a, b)))
#define __scanf(a, b) __attribute__((__format__(scanf, a, b)))
#define __deprecated
#define __must_check __attribute__((__warn_unused_result__))
#define __maybe_unused __attribute__((__unused__))
#define __always_unused __attribute__((__unused__))
#define __used __attribute__((__used__))
#define __cold __attribute__((__cold__))
#define __pure __attribute__((__pure__))
#define __const __attribute__((__const__))
#define __weak __attribute__((__weak__))
#define __section(s) __attribute__((__section__(s)))
#define __malloc __attribute__((__malloc__))
#define __alloc_size(...)
#define __realloc_size(...)
#define __counted_by(m)
#define __nonstring
#define __randomize_layout
#define __no_randomize_layout
#define __designated_init
#define __init
#define __exit
#define __initdata
#define __initconst
#define __exitdata
#define __ro_after_init
#define __read_mostly
#define __iomem
#define __user
#define __kernel
#define __force
#define __rcu
#define __percpu
#define __bitwise
#define __private
#define __acquires(x)
#define __releases(x)
#define __must_hold(x)
#define __cond_acquires(x)
#define __acquire(x) (void)0
#define __release(x) (void)0
#define __safe
#define __visible
#define __no_sanitize_address
#define __no_kcsan
#define __nocfi
#define notrace
#define __stringify_1(x...) #x
#define __stringify(x...) __stringify_1(x)
#define __concat(a, b) a##b
#define __PASTE(a, b) __concat(a, b)
#define __UNIQUE_ID(prefix) __PASTE(__PASTE(__UNIQUE_ID_, prefix), __COUNTER__)
#define fallthrough __attribute__((__fallthrough__))
#define likely(x) __builtin_expect(!!(x), 1)
#define unlikely(x) __builtin_expect(!!(x), 0)
#define barrier() __asm__ __volatile__("" ::: "memory")
#define unreachable() __builtin_unreachable()
#define __same_type(a, b) __builtin_types_compatible_p(typeof(a), typeof(b))
#define __must_be_array(a) 0
#define __is_constexpr(x) __builtin_constant_p(x)
#define __native_word(t) (sizeof(t) == 1 || sizeof(t) == 2 || sizeof(t) == 4 || sizeof(t) == 8)
#define asm __asm__
#define inline inline
#define __inline__ inline
#define __inline inline
#define noinline_for_stack noinline

/* Fails to compile when cond is a true constant; non-constant conditions (which Linux
 * leaves to the optimizer) compile to nothing. */
#define BUILD_BUG_ON(cond) ((void)sizeof(char[1 - 2 * !!(__builtin_constant_p(cond) && (cond))]))
#define array_index_nospec(index, size) (index)
#define BUILD_BUG_ON_MSG(cond, msg) _Static_assert(!(cond), msg)
#define BUILD_BUG_ON_ZERO(e) ((int)(sizeof(struct { int : (-!!(e)); })))
#define BUILD_BUG_ON_INVALID(e) ((void)(sizeof((long)(e))))
#define BUILD_BUG() do { } while (0)
#define static_assert(expr, ...) _Static_assert(expr, #expr)

/* ---- Common macros ----------------------------------------------------- */

#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define container_of(ptr, type, member) ((type *)((char *)(ptr) - offsetof(type, member)))
#define container_of_const(ptr, type, member) container_of(ptr, type, member)
#define sizeof_field(t, m) sizeof(((t *)0)->m)
#define typeof_member(t, m) typeof(((t *)0)->m)
#define offsetofend(t, m) (offsetof(t, m) + sizeof_field(t, m))
/* Sizes saturate at (~(size_t)0) on overflow, so an allocation of that size fails. */
#define check_add_overflow(a, b, d) __builtin_add_overflow(a, b, d)
#define check_sub_overflow(a, b, d) __builtin_sub_overflow(a, b, d)
#define check_mul_overflow(a, b, d) __builtin_mul_overflow(a, b, d)
static inline size_t size_mul(size_t a, size_t b) { size_t r; return __builtin_mul_overflow(a, b, &r) ? (~(size_t)0) : r; }
static inline size_t size_add(size_t a, size_t b) { size_t r; return __builtin_add_overflow(a, b, &r) ? (~(size_t)0) : r; }
static inline size_t size_sub(size_t a, size_t b) { return (a == (~(size_t)0) || b == (~(size_t)0) || a < b) ? (~(size_t)0) : a - b; }
#define struct_size(p, member, count) size_add(sizeof(*(p)), size_mul(sizeof(*(p)->member), (size_t)(count)))
#define struct_size_t(type, member, count) size_add(sizeof(type), size_mul(sizeof(((type *)0)->member[0]), (size_t)(count)))
#define flex_array_size(p, member, count) size_mul(sizeof(*(p)->member), (size_t)(count))
#define array_size(a, b) size_mul(a, b)
#define array3_size(a, b, c) size_mul(size_mul(a, b), c)
#define DECLARE_FLEX_ARRAY(type, name) struct { struct { } __empty_##name; type name[]; }

#define min(a, b) ({ typeof(a) _a = (a); typeof(b) _b = (b); _a < _b ? _a : _b; })
#define max(a, b) ({ typeof(a) _a = (a); typeof(b) _b = (b); _a > _b ? _a : _b; })
#define min3(a, b, c) min(min(a, b), c)
#define max3(a, b, c) max(max(a, b), c)
#define min_t(t, a, b) ({ t _a = (a); t _b = (b); _a < _b ? _a : _b; })
#define MAX_T(t, a, b) ((t)(a) > (t)(b) ? (t)(a) : (t)(b))
#define MIN_T(t, a, b) ((t)(a) < (t)(b) ? (t)(a) : (t)(b))
#define BITS_PER_TYPE(t) (sizeof(t) * 8)
#define max_t(t, a, b) ({ t _a = (a); t _b = (b); _a > _b ? _a : _b; })
#define umin(a, b) min_t(unsigned long long, a, b)
#define umax(a, b) max_t(unsigned long long, a, b)
#define clamp(v, lo, hi) min(max(v, lo), hi)
#define clamp_t(t, v, lo, hi) min_t(t, max_t(t, v, lo), hi)
#define clamp_val(v, lo, hi) clamp_t(typeof(v), v, lo, hi)
#define min_not_zero(a, b) ({ typeof(a) _a = (a); typeof(b) _b = (b); _a == 0 ? _b : (_b == 0 ? _a : min(_a, _b)); })
#define swap(a, b) do { typeof(a) _t = (a); (a) = (b); (b) = _t; } while (0)
#define abs(x) ({ typeof(x) _x = (x); _x < 0 ? -_x : _x; })
#define abs_diff(a, b) ({ typeof(a) _a = (a); typeof(b) _b = (b); _a > _b ? _a - _b : _b - _a; })
#define in_range(v, start, len) ((v) >= (start) && (v) < (start) + (len))

#define DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#define DIV_ROUND_UP_ULL(n, d) DIV_ROUND_UP((unsigned long long)(n), (d))
#define DIV_ROUND_CLOSEST(x, d) ({ typeof(x) _x = (x); typeof(d) _d = (d); \
    (((typeof(x))-1) > 0 || ((typeof(d))-1) > 0 || (_x > 0) == (_d > 0)) ? ((_x + (_d / 2)) / _d) : ((_x - (_d / 2)) / _d); })
#define DIV_ROUND_CLOSEST_ULL(x, d) ({ unsigned long long _t = (x) + (d) / 2; _t / (d); })
#define DIV_ROUND_DOWN_ULL(ll, d) ({ unsigned long long _t = (ll); _t / (d); })
#define roundup(x, y) ((((x) + ((y) - 1)) / (y)) * (y))
#define rounddown(x, y) ((x) - ((x) % (y)))
#define round_up(x, y) ((((x) - 1) | ((typeof(x))((y) - 1))) + 1)
#define round_down(x, y) ((x) & ~((typeof(x))((y) - 1)))
#define ALIGN(x, a) (((x) + ((typeof(x))(a) - 1)) & ~((typeof(x))(a) - 1))
#define ALIGN_DOWN(x, a) ((x) & ~((typeof(x))(a) - 1))
#define PTR_ALIGN(p, a) ((typeof(p))ALIGN((unsigned long)(p), (a)))
#define IS_ALIGNED(x, a) (((x) & ((typeof(x))(a) - 1)) == 0)
#define mult_frac(x, n, d) ({ typeof(x) _q = (x) / (d); typeof(x) _r = (x) % (d); (_q * (n)) + ((_r * (n)) / (d)); })

#define BIT(n) (1UL << (n))
#define BIT_ULL(n) (1ULL << (n))
#define BIT_MASK(n) (1UL << ((n) % 64))
#define BIT_WORD(n) ((n) / 64)
#define BITS_PER_LONG 64
#define BITS_PER_LONG_LONG 64
#define BITS_PER_BYTE 8
#define BITS_TO_LONGS(n) DIV_ROUND_UP(n, BITS_PER_LONG)
#define GENMASK(h, l) (((~0UL) - (1UL << (l)) + 1) & (~0UL >> (BITS_PER_LONG - 1 - (h))))
#define GENMASK_ULL(h, l) (((~0ULL) - (1ULL << (l)) + 1) & (~0ULL >> (63 - (h))))
#define lower_32_bits(n) ((u32)((n) & 0xffffffff))
#define upper_32_bits(n) ((u32)(((n) >> 16) >> 16))
#define U8_MAX ((u8)~0U)
#define S8_MAX ((s8)(U8_MAX >> 1))
#define S8_MIN ((s8)(-S8_MAX - 1))
#define U16_MAX ((u16)~0U)
#define S16_MAX ((s16)(U16_MAX >> 1))
#define S16_MIN ((s16)(-S16_MAX - 1))
#define U32_MAX ((u32)~0U)
#define S32_MAX ((s32)(U32_MAX >> 1))
#define S32_MIN ((s32)(-S32_MAX - 1))
#define U64_MAX ((u64)~0ULL)
#define S64_MAX ((s64)(U64_MAX >> 1))
#define S64_MIN ((s64)(-S64_MAX - 1))
#define INT_MAX ((int)(~0U >> 1))
#define INT_MIN (-INT_MAX - 1)
#define UINT_MAX (~0U)
#define LONG_MAX ((long)(~0UL >> 1))
#define LONG_MIN (-LONG_MAX - 1)
#define ULONG_MAX (~0UL)
#define LLONG_MAX ((long long)(~0ULL >> 1))
#define ULLONG_MAX (~0ULL)
#define SIZE_MAX (~(size_t)0)
#define SHRT_MAX ((short)(USHRT_MAX >> 1))
#define USHRT_MAX ((unsigned short)~0U)
#define type_max(t) ((t)((t)-1 > 0 ? (t)-1 : (t)(((1ULL << (sizeof(t) * 8 - 2)) - 1) * 2 + 1)))
#define type_min(t) ((t)-type_max(t) - 1)

#define KB (1024)
#define SZ_1K 0x00000400
#define SZ_4K 0x00001000
#define SZ_64K 0x00010000
#define SZ_1M 0x00100000
#define SZ_2M 0x00200000
#define SZ_16M 0x01000000
#define SZ_256M 0x10000000
#define SZ_1G 0x40000000
#define SZ_4G 0x100000000ULL

#define PAGE_SHIFT 12
#define PAGE_SIZE (1UL << PAGE_SHIFT)
#define PAGE_MASK (~(PAGE_SIZE - 1))
#define PAGE_ALIGN(x) ALIGN(x, PAGE_SIZE)
#define PFN_UP(x) (((x) + PAGE_SIZE - 1) >> PAGE_SHIFT)
#define PFN_DOWN(x) ((x) >> PAGE_SHIFT)
#define PFN_PHYS(x) ((phys_addr_t)(x) << PAGE_SHIFT)

/* ---- Once-only reads and writes ----------------------------------------- */

#define READ_ONCE(x) (*(const volatile typeof(x) *)&(x))
#define WRITE_ONCE(x, v) do { *(volatile typeof(x) *)&(x) = (v); } while (0)
#define data_race(expr) (expr)
#define ACCESS_PRIVATE(p, member) ((p)->member)

/* ---- Error pointers ------------------------------------------------------ */

#define MAX_ERRNO 4095
#define IS_ERR_VALUE(x) unlikely((unsigned long)(void *)(x) >= (unsigned long)-MAX_ERRNO)
static inline void *ERR_PTR(long error) { return (void *)error; }
static inline long PTR_ERR(const void *ptr) { return (long)ptr; }
static inline bool IS_ERR(const void *ptr) { return IS_ERR_VALUE((unsigned long)ptr); }
static inline bool IS_ERR_OR_NULL(const void *ptr) { return !ptr || IS_ERR_VALUE((unsigned long)ptr); }
static inline void *ERR_CAST(const void *ptr) { return (void *)ptr; }
static inline int PTR_ERR_OR_ZERO(const void *ptr) { return IS_ERR(ptr) ? (int)PTR_ERR(ptr) : 0; }

/* ---- Modules and exports (everything is built in) ------------------------ */

#define THIS_MODULE ((struct module *)0)
#define EXPORT_SYMBOL(sym)
#define EXPORT_SYMBOL_GPL(sym)
#define EXPORT_SYMBOL_NS(sym, ns)
#define EXPORT_SYMBOL_NS_GPL(sym, ns)
#define EXPORT_SYMBOL_FOR_TESTS_ONLY(sym)
#define EXPORT_SYMBOL_IF_KUNIT(sym)
/*
 * A module's init and exit functions become waveos_init_<fn> and
 * waveos_exit_<fn>, which WaveOS calls in a fixed order (see
 * kernel/src/linux/mod.rs).
 */
#define module_init(fn) int waveos_init_##fn(void); int waveos_init_##fn(void) { return fn(); }
#define module_exit(fn) void waveos_exit_##fn(void); void waveos_exit_##fn(void) { fn(); }
#define subsys_initcall module_init
#define late_initcall module_init
#define MODULE_LICENSE(x)
#define MODULE_AUTHOR(x)
#define MODULE_DESCRIPTION(x)
#define MODULE_DEVICE_TABLE(type, name)
#define MODULE_FIRMWARE(x)
#define MODULE_IMPORT_NS(x)
#define MODULE_ALIAS(x)
#define MODULE_SOFTDEP(x)
#define MODULE_VERSION(x)
#define MODULE_PARM_DESC(name, desc)
#define module_param(name, type, perm)
#define module_param_named(name, value, type, perm)
#define module_param_unsafe(name, type, perm)
#define module_param_named_unsafe(name, value, type, perm)
#define module_param_string(name, string, len, perm)
#define __MODULE_STRING(x) __stringify(x)
struct module;
static inline bool try_module_get(struct module *m) { (void)m; return true; }
static inline void module_put(struct module *m) { (void)m; }
#define symbol_get(x) (&(x))
#define symbol_put(x) do { } while (0)
#define KBUILD_MODNAME "drm"

#endif
