/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: printing, warnings and assertions. Messages go to the
 * WaveOS kernel log; the KERN_* level prefix becomes the log category.
 */
#ifndef _WAVEOS_PRINTK_H
#define _WAVEOS_PRINTK_H

#include <waveos/base.h>

#define KERN_SOH "\001"
#define KERN_EMERG KERN_SOH "0"
#define KERN_ALERT KERN_SOH "1"
#define KERN_CRIT KERN_SOH "2"
#define KERN_ERR KERN_SOH "3"
#define KERN_WARNING KERN_SOH "4"
#define KERN_NOTICE KERN_SOH "5"
#define KERN_INFO KERN_SOH "6"
#define KERN_DEBUG KERN_SOH "7"
#define KERN_DEFAULT ""
#define KERN_CONT KERN_SOH "c"
#define LOGLEVEL_DEFAULT -1
#define LOGLEVEL_ERR 3
#define LOGLEVEL_DEBUG 7

/* %pV: a nested format and its arguments. */
struct va_format {
    const char *fmt;
    va_list *va;
};
#define _RET_IP_ ((unsigned long)__builtin_return_address(0))
#define _THIS_IP_ ({ __label__ __here; __here: (unsigned long)&&__here; })

int printk(const char *fmt, ...) __printf(1, 2);
int vprintk(const char *fmt, va_list args);
int vsnprintf(char *buf, size_t size, const char *fmt, va_list args) __printf(3, 0);
int snprintf(char *buf, size_t size, const char *fmt, ...) __printf(3, 4);
int vscnprintf(char *buf, size_t size, const char *fmt, va_list args) __printf(3, 0);
int scnprintf(char *buf, size_t size, const char *fmt, ...) __printf(3, 4);
int sprintf(char *buf, const char *fmt, ...) __printf(2, 3);
int vsprintf(char *buf, const char *fmt, va_list args) __printf(2, 0);
int sscanf(const char *buf, const char *fmt, ...) __scanf(2, 3);
char *kasprintf(gfp_t gfp, const char *fmt, ...) __printf(2, 3);
char *kvasprintf(gfp_t gfp, const char *fmt, va_list args) __printf(2, 0);
const char *kvasprintf_const(gfp_t gfp, const char *fmt, va_list args) __printf(2, 0);
void waveos_backtrace(void);

#define no_printk(fmt, ...) ({ if (0) printk(fmt, ##__VA_ARGS__); 0; })
#define pr_fmt(fmt) fmt
#define pr_emerg(fmt, ...) printk(KERN_EMERG pr_fmt(fmt), ##__VA_ARGS__)
#define pr_alert(fmt, ...) printk(KERN_ALERT pr_fmt(fmt), ##__VA_ARGS__)
#define pr_crit(fmt, ...) printk(KERN_CRIT pr_fmt(fmt), ##__VA_ARGS__)
#define pr_err(fmt, ...) printk(KERN_ERR pr_fmt(fmt), ##__VA_ARGS__)
#define pr_warn(fmt, ...) printk(KERN_WARNING pr_fmt(fmt), ##__VA_ARGS__)
#define pr_warning pr_warn
#define pr_notice(fmt, ...) printk(KERN_NOTICE pr_fmt(fmt), ##__VA_ARGS__)
#define pr_info(fmt, ...) printk(KERN_INFO pr_fmt(fmt), ##__VA_ARGS__)
#define pr_cont(fmt, ...) printk(KERN_CONT fmt, ##__VA_ARGS__)
#define pr_debug(fmt, ...) no_printk(KERN_DEBUG pr_fmt(fmt), ##__VA_ARGS__)
#define pr_devel(fmt, ...) no_printk(KERN_DEBUG pr_fmt(fmt), ##__VA_ARGS__)
#define pr_err_once pr_err
#define pr_warn_once pr_warn
#define pr_info_once pr_info
#define pr_notice_once pr_notice
#define pr_debug_once pr_debug
#define pr_warn_ratelimited pr_warn
#define pr_err_ratelimited pr_err
#define pr_info_ratelimited pr_info
#define pr_debug_ratelimited pr_debug
#define printk_once printk
#define printk_ratelimited printk
#define printk_ratelimit() 1

/* Prints a warning (once per call site for the _ONCE forms) when cond holds. */
int waveos_warn(const char *file, int line, const char *cond);
#define WARN_ON(cond) ({ int __c = !!(cond); if (unlikely(__c)) waveos_warn(__FILE__, __LINE__, #cond); unlikely(__c); })
#define WARN_ON_ONCE(cond) ({ static bool __done; int __c = !!(cond); \
    if (unlikely(__c) && !__done) { __done = true; waveos_warn(__FILE__, __LINE__, #cond); } unlikely(__c); })
#define WARN(cond, fmt, ...) ({ int __c = !!(cond); if (unlikely(__c)) { printk(KERN_WARNING fmt, ##__VA_ARGS__); \
    waveos_warn(__FILE__, __LINE__, #cond); } unlikely(__c); })
#define WARN_ONCE(cond, fmt, ...) ({ static bool __done; int __c = !!(cond); \
    if (unlikely(__c) && !__done) { __done = true; printk(KERN_WARNING fmt, ##__VA_ARGS__); waveos_warn(__FILE__, __LINE__, #cond); } unlikely(__c); })
#define WARN_TAINT(cond, taint, fmt, ...) WARN(cond, fmt, ##__VA_ARGS__)

void waveos_bug(const char *file, int line) __noreturn;
#define BUG() waveos_bug(__FILE__, __LINE__)
#define BUG_ON(cond) do { if (unlikely(cond)) BUG(); } while (0)
#define VM_BUG_ON(cond) BUG_ON(cond)
#define might_sleep() do { } while (0)
#define might_sleep_if(c) do { } while (0)
#define might_fault() do { } while (0)
#define might_lock(l) do { } while (0)
#define cant_sleep() do { } while (0)
#define dump_stack() waveos_backtrace()
#define add_taint(flag, lockdep) do { } while (0)
#define TAINT_WARN 9
#define LOCKDEP_STILL_OK 0
#define print_hex_dump(level, prefix, type, rowsize, groupsize, buf, len, ascii) do { } while (0)
#define print_hex_dump_debug(...) do { } while (0)
#define DUMP_PREFIX_NONE 0
#define DUMP_PREFIX_OFFSET 2

#endif

#define DECLARE_DYNDBG_CLASSMAP(...)
#define DYNAMIC_DEBUG_BRANCH(d) 0
