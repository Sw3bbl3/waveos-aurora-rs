/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: time. jiffies tick at HZ (1000) from the WaveOS clock;
 * ktime_t is nanoseconds since boot; sleeps and delays use the scheduler.
 */
#ifndef _WAVEOS_TIME_H
#define _WAVEOS_TIME_H

#include <waveos/sync.h>

#define HZ 1000
#define USER_HZ 100
#define MSEC_PER_SEC 1000L
#define USEC_PER_MSEC 1000L
#define NSEC_PER_USEC 1000L
#define NSEC_PER_MSEC 1000000L
#define USEC_PER_SEC 1000000L
#define NSEC_PER_SEC 1000000000L
#define PSEC_PER_SEC 1000000000000LL
#define FSEC_PER_SEC 1000000000000000LL
#define MAX_JIFFY_OFFSET ((LONG_MAX >> 1) - 1)
#define MAX_SCHEDULE_TIMEOUT LONG_MAX

typedef s64 ktime_t;
struct timespec64 {
    time64_t tv_sec;
    long tv_nsec;
};

static inline unsigned long waveos_jiffies(void) { return (unsigned long)(waveos_now_ns() / 1000000ULL); }
#define jiffies waveos_jiffies()
#define jiffies_64 ((u64)waveos_jiffies())
#define get_jiffies_64() ((u64)waveos_jiffies())
#define INITIAL_JIFFIES 0
static inline unsigned int jiffies_to_msecs(unsigned long j) { return (unsigned int)j; }
static inline unsigned int jiffies_to_usecs(unsigned long j) { return (unsigned int)(j * 1000); }
static inline u64 jiffies_to_nsecs(unsigned long j) { return (u64)j * 1000000ULL; }
static inline unsigned long msecs_to_jiffies(unsigned int m) { return m; }
static inline unsigned long usecs_to_jiffies(unsigned int u) { return (u + 999) / 1000; }
static inline unsigned long nsecs_to_jiffies(u64 n) { return (unsigned long)(n / 1000000ULL); }
static inline u64 nsecs_to_jiffies64(u64 n) { return n / 1000000ULL; }
#define time_after(a, b) ((long)((b) - (a)) < 0)
#define time_before(a, b) time_after(b, a)
#define time_after_eq(a, b) ((long)((a) - (b)) >= 0)
#define time_before_eq(a, b) time_after_eq(b, a)
#define time_is_after_jiffies(a) time_before(jiffies, a)
#define time_is_before_jiffies(a) time_after(jiffies, a)
#define time_in_range(a, b, c) (time_after_eq(a, b) && time_before_eq(a, c))
#define time_after64 time_after
#define time_before64 time_before

static inline ktime_t ktime_get(void) { return (ktime_t)waveos_now_ns(); }
#define ktime_get_raw ktime_get
#define ktime_get_mono_fast_ns() waveos_now_ns()
#define ktime_get_boottime ktime_get
#define ktime_get_real ktime_get
#define ktime_get_ns() ((u64)waveos_now_ns())
#define ktime_get_raw_ns() ((u64)waveos_now_ns())
#define ktime_get_boottime_ns() ((u64)waveos_now_ns())
#define ktime_get_real_ns() ((u64)waveos_now_ns())
static inline time64_t ktime_get_seconds(void) { return (time64_t)(waveos_now_ns() / 1000000000ULL); }
static inline time64_t ktime_get_real_seconds(void) { return ktime_get_seconds(); }
static inline void ktime_get_ts64(struct timespec64 *ts)
{ u64 n = waveos_now_ns(); ts->tv_sec = n / 1000000000ULL; ts->tv_nsec = n % 1000000000ULL; }
#define ktime_get_real_ts64 ktime_get_ts64
#define ktime_set(s, ns) ((ktime_t)(s) * NSEC_PER_SEC + (ns))
#define ktime_add(a, b) ((a) + (b))
#define ktime_sub(a, b) ((a) - (b))
#define ktime_add_ns(k, n) ((k) + (ktime_t)(n))
#define ktime_sub_ns(k, n) ((k) - (ktime_t)(n))
#define ktime_add_us(k, u) ((k) + (ktime_t)(u) * NSEC_PER_USEC)
#define ktime_add_ms(k, m) ((k) + (ktime_t)(m) * NSEC_PER_MSEC)
#define ktime_compare(a, b) ((a) < (b) ? -1 : (a) > (b) ? 1 : 0)
#define ktime_after(a, b) ((a) > (b))
#define ktime_before(a, b) ((a) < (b))
#define ktime_to_ns(k) ((s64)(k))
#define ktime_to_us(k) ((s64)(k) / NSEC_PER_USEC)
#define ktime_to_ms(k) ((s64)(k) / NSEC_PER_MSEC)
#define ktime_us_delta(a, b) ktime_to_us((a) - (b))
#define ktime_ms_delta(a, b) ktime_to_ms((a) - (b))
#define ns_to_ktime(n) ((ktime_t)(n))
#define us_to_ktime(u) ((ktime_t)(u) * NSEC_PER_USEC)
#define ms_to_ktime(m) ((ktime_t)(m) * NSEC_PER_MSEC)
#define KTIME_MAX ((s64)~((u64)1 << 63))
static inline struct timespec64 ktime_to_timespec64(ktime_t k)
{ struct timespec64 t = { k / NSEC_PER_SEC, k % NSEC_PER_SEC }; return t; }
static inline ktime_t timespec64_to_ktime(struct timespec64 t) { return ktime_set(t.tv_sec, t.tv_nsec); }
static inline s64 timespec64_to_ns(const struct timespec64 *t) { return (s64)t->tv_sec * NSEC_PER_SEC + t->tv_nsec; }
static inline struct timespec64 ns_to_timespec64(s64 n) { return ktime_to_timespec64(n); }

static inline void msleep(unsigned int ms) { waveos_sleep_us((u64)ms * 1000); }
static inline unsigned long msleep_interruptible(unsigned int ms) { msleep(ms); return 0; }
static inline void ssleep(unsigned int s) { msleep(s * 1000); }
static inline void usleep_range(unsigned long min, unsigned long max) { (void)max; waveos_sleep_us(min); }
static inline void fsleep(unsigned long us) { waveos_sleep_us(us); }
static inline void udelay(unsigned long us)
{ u64 end = waveos_now_ns() + (u64)us * 1000; while (waveos_now_ns() < end) cpu_relax(); }
static inline void ndelay(unsigned long ns)
{ u64 end = waveos_now_ns() + ns; while (waveos_now_ns() < end) cpu_relax(); }
static inline void mdelay(unsigned long ms) { udelay(ms * 1000); }
static inline long schedule_timeout(long t) { if (t > 0 && t != MAX_SCHEDULE_TIMEOUT) msleep((unsigned int)t); else waveos_yield(); return 0; }
#define schedule_timeout_uninterruptible schedule_timeout
#define schedule_timeout_interruptible schedule_timeout
#define schedule() waveos_yield()
#define cond_resched() waveos_yield()
#define yield() waveos_yield()

#endif
