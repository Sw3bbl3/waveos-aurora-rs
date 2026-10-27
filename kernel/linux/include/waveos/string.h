/* SPDX-License-Identifier: MIT */
/* WaveOS LinuxKPI: memory and string functions, and character classes. */
#ifndef _WAVEOS_STRING_H
#define _WAVEOS_STRING_H

#include <waveos/base.h>

void *memcpy(void *dst, const void *src, size_t n);
void *memmove(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);
void *memchr(const void *s, int c, size_t n);
void *memchr_inv(const void *s, int c, size_t n);
static inline bool mem_is_zero(const void *s, size_t n) { return !memchr_inv(s, 0, n); }
/* Copies a C string into a fixed-size field, padding the rest with `pad`. */
#define memcpy_and_pad(dest, dest_len, src, count, pad)                                                \
    do {                                                                                               \
        size_t _dl = (dest_len), _c = (count);                                                         \
        if (_dl > _c) {                                                                                \
            memcpy(dest, src, _c);                                                                     \
            memset((char *)(dest) + _c, pad, _dl - _c);                                                \
        } else {                                                                                       \
            memcpy(dest, src, _dl);                                                                    \
        }                                                                                              \
    } while (0)
#define strtomem_pad(dest, src, pad) memcpy_and_pad(dest, sizeof(dest), src, strnlen(src, sizeof(dest)), pad)
#define strtomem(dest, src) memcpy(dest, src, strnlen(src, sizeof(dest)) < sizeof(dest) ? strnlen(src, sizeof(dest)) : sizeof(dest))
size_t strlen(const char *s);
size_t strnlen(const char *s, size_t max);
int strcmp(const char *a, const char *b);
int strncmp(const char *a, const char *b, size_t n);
int strcasecmp(const char *a, const char *b);
int strncasecmp(const char *a, const char *b, size_t n);
char *strcpy(char *dst, const char *src);
char *strncpy(char *dst, const char *src, size_t n);
size_t strlcpy(char *dst, const char *src, size_t size);
ssize_t strscpy(char *dst, const char *src, size_t size);
ssize_t strscpy_pad(char *dst, const char *src, size_t size);
size_t strlcat(char *dst, const char *src, size_t size);
char *strcat(char *dst, const char *src);
char *strncat(char *dst, const char *src, size_t n);
char *strchr(const char *s, int c);
char *strnchr(const char *s, size_t n, int c);
char *strrchr(const char *s, int c);
char *strchrnul(const char *s, int c);
char *strstr(const char *haystack, const char *needle);
char *strnstr(const char *haystack, const char *needle, size_t len);
char *strsep(char **s, const char *delim);
char *strpbrk(const char *s, const char *accept);
size_t strspn(const char *s, const char *accept);
size_t strcspn(const char *s, const char *reject);
char *skip_spaces(const char *s);
char *strim(char *s);
static inline char *strstrip(char *s) { return strim(s); }
bool sysfs_streq(const char *a, const char *b);
int match_string(const char *const *array, size_t n, const char *string);
#define sysfs_match_string(a, s) match_string(a, ARRAY_SIZE(a), s)
static inline bool str_has_prefix(const char *s, const char *prefix) { return strncmp(s, prefix, strlen(prefix)) == 0; }
static inline bool strstarts(const char *s, const char *prefix) { return str_has_prefix(s, prefix); }
static inline const char *str_yes_no(bool v) { return v ? "yes" : "no"; }
static inline const char *str_on_off(bool v) { return v ? "on" : "off"; }
static inline const char *str_enable_disable(bool v) { return v ? "enable" : "disable"; }
static inline const char *str_enabled_disabled(bool v) { return v ? "enabled" : "disabled"; }
static inline const char *str_true_false(bool v) { return v ? "true" : "false"; }
static inline const char *str_plural(size_t n) { return n == 1 ? "" : "s"; }
#define memset_after(obj, v, member) memset((u8 *)(obj) + offsetofend(typeof(*(obj)), member), v, \
    sizeof(*(obj)) - offsetofend(typeof(*(obj)), member))
#define memset_startat(obj, v, member) memset((u8 *)(obj) + offsetof(typeof(*(obj)), member), v, \
    sizeof(*(obj)) - offsetof(typeof(*(obj)), member))
#define unsafe_memcpy(dst, src, n, justification) memcpy(dst, src, n)
#define memcpy_and_pad(dst, dlen, src, slen, pad) do { size_t _c = min_t(size_t, dlen, slen); \
    memcpy(dst, src, _c); if ((dlen) > _c) memset((u8 *)(dst) + _c, pad, (dlen) - _c); } while (0)

int kstrtoint(const char *s, unsigned int base, int *res);
int kstrtouint(const char *s, unsigned int base, unsigned int *res);
int kstrtol(const char *s, unsigned int base, long *res);
int kstrtoul(const char *s, unsigned int base, unsigned long *res);
int kstrtoll(const char *s, unsigned int base, long long *res);
int kstrtoull(const char *s, unsigned int base, unsigned long long *res);
int kstrtobool(const char *s, bool *res);
static inline int kstrtou8(const char *s, unsigned int base, u8 *res)
{ unsigned int v; int e = kstrtouint(s, base, &v); if (!e) { if (v > 0xff) return -34; *res = v; } return e; }
static inline int kstrtou32(const char *s, unsigned int base, u32 *res) { return kstrtouint(s, base, res); }
static inline int kstrtos32(const char *s, unsigned int base, s32 *res) { return kstrtoint(s, base, res); }
unsigned long simple_strtoul(const char *cp, char **endp, unsigned int base);
long simple_strtol(const char *cp, char **endp, unsigned int base);
unsigned long long simple_strtoull(const char *cp, char **endp, unsigned int base);
long long simple_strtoll(const char *cp, char **endp, unsigned int base);

/* ---- ctype ---- */
#define _WAVEOS_U 0x01
#define _WAVEOS_L 0x02
#define _WAVEOS_D 0x04
#define _WAVEOS_C 0x08
#define _WAVEOS_P 0x10
#define _WAVEOS_S 0x20
#define _WAVEOS_X 0x40
#define _WAVEOS_SP 0x80
extern const unsigned char _waveos_ctype[256];
#define __ismask(c) (_waveos_ctype[(unsigned char)(c)])
#define isalnum(c) ((__ismask(c) & (_WAVEOS_U | _WAVEOS_L | _WAVEOS_D)) != 0)
#define isalpha(c) ((__ismask(c) & (_WAVEOS_U | _WAVEOS_L)) != 0)
#define iscntrl(c) ((__ismask(c) & (_WAVEOS_C)) != 0)
#define isdigit(c) ((unsigned)((c) - '0') < 10)
#define isgraph(c) ((__ismask(c) & (_WAVEOS_P | _WAVEOS_U | _WAVEOS_L | _WAVEOS_D)) != 0)
#define islower(c) ((__ismask(c) & (_WAVEOS_L)) != 0)
#define isprint(c) ((__ismask(c) & (_WAVEOS_P | _WAVEOS_U | _WAVEOS_L | _WAVEOS_D | _WAVEOS_SP)) != 0)
#define ispunct(c) ((__ismask(c) & (_WAVEOS_P)) != 0)
#define isspace(c) ((__ismask(c) & (_WAVEOS_S)) != 0)
#define isupper(c) ((__ismask(c) & (_WAVEOS_U)) != 0)
#define isxdigit(c) ((__ismask(c) & (_WAVEOS_D | _WAVEOS_X)) != 0)
#define isascii(c) (((unsigned char)(c)) <= 0x7f)
#define toascii(c) (((unsigned char)(c)) & 0x7f)
static inline unsigned char __tolower(unsigned char c) { return (c >= 'A' && c <= 'Z') ? c + 32 : c; }
static inline unsigned char __toupper(unsigned char c) { return (c >= 'a' && c <= 'z') ? c - 32 : c; }
#define tolower(c) __tolower(c)
#define toupper(c) __toupper(c)
static inline int hex_to_bin(unsigned char ch)
{
    if (ch >= '0' && ch <= '9') return ch - '0';
    ch = __tolower(ch);
    if (ch >= 'a' && ch <= 'f') return ch - 'a' + 10;
    return -1;
}
extern const char hex_asc[];
#define hex_asc_lo(x) hex_asc[((x) & 0x0f)]
#define hex_asc_hi(x) hex_asc[((x) & 0xf0) >> 4]

#endif
