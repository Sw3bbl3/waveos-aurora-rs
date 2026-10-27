// SPDX-License-Identifier: MIT
/*
 * WaveOS LinuxKPI: formatted printing.
 *
 * C99 conversions plus the %p extensions the ported drivers use: %pV (a
 * nested format), %p4cc (a DRM fourcc), %pe (an error pointer), %ph (a hex
 * dump), %pa/%pad (physical and DMA addresses) and %pR (a resource). Other
 * %p forms print the pointer. printk hands finished lines to the WaveOS
 * kernel log (waveos_log, in Rust).
 */
#include <waveos/kernel.h>
#include <waveos/fs.h>
#include <waveos/mm.h>

void waveos_log(int level, const char *msg, size_t len);

struct out {
    char *buf;
    size_t size; /* capacity, including the terminator */
    size_t len;  /* characters produced (may exceed size) */
};

static void put(struct out *o, char c)
{
    if (o->len + 1 < o->size)
        o->buf[o->len] = c;
    o->len++;
}

static void put_str(struct out *o, const char *s, size_t n)
{
    while (n--)
        put(o, *s++);
}

enum { LEFT = 1, PLUS = 2, SPACE = 4, ALT = 8, ZERO = 16, UPPER = 32, SIGNED = 64 };

static void put_padded(struct out *o, const char *s, size_t n, int width, int flags)
{
    int pad = width > (int)n ? width - (int)n : 0;
    if (!(flags & LEFT))
        while (pad-- > 0)
            put(o, ' ');
    put_str(o, s, n);
    if (flags & LEFT)
        while (pad-- > 0)
            put(o, ' ');
}

static void put_number(struct out *o, unsigned long long v, int base, int width, int prec, int flags)
{
    const char *digits = (flags & UPPER) ? "0123456789ABCDEF" : "0123456789abcdef";
    char tmp[24], sign = 0;
    const char *prefix = "";
    int n = 0, len, pad;
    if ((flags & SIGNED) && (long long)v < 0) {
        sign = '-';
        v = -(long long)v;
    } else if (flags & SIGNED) {
        sign = (flags & PLUS) ? '+' : (flags & SPACE) ? ' ' : 0;
    }
    if ((flags & ALT) && v) {
        if (base == 16)
            prefix = (flags & UPPER) ? "0X" : "0x";
        else if (base == 8)
            prefix = "0";
    }
    do {
        tmp[n++] = digits[v % base];
        v /= base;
    } while (v);
    if (prec == 0 && n == 1 && tmp[0] == '0')
        n = 0;
    len = (prec > n ? prec : n) + (sign ? 1 : 0) + (int)strlen(prefix);
    pad = width > len ? width - len : 0;
    if (!(flags & LEFT) && !((flags & ZERO) && prec < 0))
        for (; pad > 0; pad--)
            put(o, ' ');
    if (sign)
        put(o, sign);
    put_str(o, prefix, strlen(prefix));
    if ((flags & ZERO) && !(flags & LEFT) && prec < 0)
        for (; pad > 0; pad--)
            put(o, '0');
    for (int z = n; z < prec; z++)
        put(o, '0');
    while (n)
        put(o, tmp[--n]);
    for (; pad > 0; pad--)
        put(o, ' ');
}

static void format(struct out *o, const char *fmt, va_list ap);

/* The %p extensions. Returns the format position after the extension. */
static const char *put_pointer(struct out *o, const char *fmt, void *p, int width, int flags)
{
    char buf[48];
    switch (*fmt) {
    case 'V': {
        struct va_format *vaf = p;
        va_list copy;
        va_copy(copy, *vaf->va);
        format(o, vaf->fmt, copy);
        va_end(copy);
        return fmt + 1;
    }
    case '4':
        if (fmt[1] == 'c' && fmt[2] == 'c') {
            u32 cc = *(const u32 *)p;
            for (int i = 0; i < 4; i++) {
                char c = (cc >> (8 * i)) & 0x7f;
                put(o, isprint(c) ? c : '.');
            }
            snprintf(buf, sizeof(buf), " %s-endian (0x%08x)", (cc & (1u << 31)) ? "big" : "little", cc);
            put_str(o, buf, strlen(buf));
            return fmt + 3;
        }
        break;
    case 'e': {
        long err = (long)p;
        snprintf(buf, sizeof(buf), "%ld", err);
        put_padded(o, buf, strlen(buf), width, flags);
        return fmt + 1;
    }
    case 'h': {
        /* Hex dump: %*ph with the length as the field width (default 1). */
        const unsigned char *b = p;
        char sep = ' ';
        int n = width > 0 ? width : 1;
        fmt++;
        if (*fmt == 'C' || *fmt == 'D' || *fmt == 'N') {
            sep = *fmt == 'C' ? ':' : *fmt == 'D' ? '-' : 0;
            fmt++;
        }
        for (int i = 0; i < n && i < 64; i++) {
            if (i && sep)
                put(o, sep);
            put(o, hex_asc[b[i] >> 4]);
            put(o, hex_asc[b[i] & 15]);
        }
        return fmt;
    }
    case 'a': {
        u64 addr = fmt[1] == 'd' ? *(const dma_addr_t *)p : *(const phys_addr_t *)p;
        put_number(o, addr, 16, 18, -1, ALT | ZERO);
        return fmt + ((fmt[1] == 'd' || fmt[1] == 'p') ? 2 : 1);
    }
    case 'R':
    case 'r': {
        const struct resource *r = p;
        snprintf(buf, sizeof(buf), "[mem %#010llx-%#010llx]", (unsigned long long)r->start,
                 (unsigned long long)r->end);
        put_str(o, buf, strlen(buf));
        return fmt + 1;
    }
    case 'S':
    case 's':
    case 'B':
    case 'K':
    case 'x':
        put_number(o, (unsigned long)p, 16, width, -1, ALT | flags);
        return fmt + 1;
    }
    put_number(o, (unsigned long)p, 16, width, -1, ALT | flags);
    return fmt;
}

static void format(struct out *o, const char *fmt, va_list ap)
{
    while (*fmt) {
        int flags = 0, width = -1, prec = -1, lng = 0, base = 10;
        unsigned long long v;
        if (*fmt != '%') {
            put(o, *fmt++);
            continue;
        }
        fmt++;
        for (;; fmt++) {
            if (*fmt == '-')
                flags |= LEFT;
            else if (*fmt == '+')
                flags |= PLUS;
            else if (*fmt == ' ')
                flags |= SPACE;
            else if (*fmt == '#')
                flags |= ALT;
            else if (*fmt == '0')
                flags |= ZERO;
            else
                break;
        }
        if (*fmt == '*') {
            width = va_arg(ap, int);
            if (width < 0) {
                flags |= LEFT;
                width = -width;
            }
            fmt++;
        } else {
            for (width = 0; isdigit(*fmt); fmt++)
                width = width * 10 + (*fmt - '0');
        }
        if (*fmt == '.') {
            fmt++;
            if (*fmt == '*') {
                prec = va_arg(ap, int);
                fmt++;
            } else {
                for (prec = 0; isdigit(*fmt); fmt++)
                    prec = prec * 10 + (*fmt - '0');
            }
        }
        for (;; fmt++) {
            if (*fmt == 'l')
                lng++;
            else if (*fmt == 'z' || *fmt == 't' || *fmt == 'j' || *fmt == 'L')
                lng = 2;
            else if (*fmt == 'h')
                lng--;
            else
                break;
        }
        switch (*fmt) {
        case 'c': {
            char c = (char)va_arg(ap, int);
            put_padded(o, &c, 1, width, flags);
            fmt++;
            continue;
        }
        case 's': {
            const char *s = va_arg(ap, const char *);
            if (!s)
                s = "(null)";
            put_padded(o, s, prec >= 0 ? strnlen(s, prec) : strlen(s), width, flags);
            fmt++;
            continue;
        }
        case 'p':
            fmt = put_pointer(o, fmt + 1, va_arg(ap, void *), width, flags);
            continue;
        case '%':
            put(o, '%');
            fmt++;
            continue;
        case 'd':
        case 'i':
            flags |= SIGNED;
            if (lng >= 2)
                v = va_arg(ap, long long);
            else if (lng == 1)
                v = va_arg(ap, long);
            else if (lng == -1)
                v = (short)va_arg(ap, int);
            else if (lng <= -2)
                v = (signed char)va_arg(ap, int);
            else
                v = va_arg(ap, int);
            break;
        case 'X':
            flags |= UPPER;
            base = 16;
            goto unsigned_number;
        case 'x':
            base = 16;
            goto unsigned_number;
        case 'o':
            base = 8;
            goto unsigned_number;
        case 'u':
        unsigned_number:
            if (lng >= 2)
                v = va_arg(ap, unsigned long long);
            else if (lng == 1)
                v = va_arg(ap, unsigned long);
            else if (lng == -1)
                v = (unsigned short)va_arg(ap, unsigned int);
            else if (lng <= -2)
                v = (unsigned char)va_arg(ap, unsigned int);
            else
                v = va_arg(ap, unsigned int);
            break;
        default:
            /* Unknown conversion: print it as written. */
            put(o, '%');
            if (*fmt)
                put(o, *fmt++);
            continue;
        }
        put_number(o, v, base, width, prec, flags);
        fmt++;
    }
}

int vsnprintf(char *buf, size_t size, const char *fmt, va_list args)
{
    struct out o = { buf, size, 0 };
    va_list copy;
    va_copy(copy, args);
    format(&o, fmt, copy);
    va_end(copy);
    if (size)
        buf[o.len < size ? o.len : size - 1] = 0;
    return o.len;
}

int snprintf(char *buf, size_t size, const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vsnprintf(buf, size, fmt, ap);
    va_end(ap);
    return n;
}

int vscnprintf(char *buf, size_t size, const char *fmt, va_list args)
{
    int n = vsnprintf(buf, size, fmt, args);
    if (!size)
        return 0;
    return (size_t)n < size ? n : (int)size - 1;
}

int scnprintf(char *buf, size_t size, const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vscnprintf(buf, size, fmt, ap);
    va_end(ap);
    return n;
}

int vsprintf(char *buf, const char *fmt, va_list args) { return vsnprintf(buf, INT_MAX, fmt, args); }

int sprintf(char *buf, const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vsnprintf(buf, INT_MAX, fmt, ap);
    va_end(ap);
    return n;
}

char *kvasprintf(gfp_t gfp, const char *fmt, va_list args)
{
    int n = vsnprintf(NULL, 0, fmt, args);
    char *s = kmalloc(n + 1, gfp);
    if (s)
        vsnprintf(s, n + 1, fmt, args);
    return s;
}

const char *kvasprintf_const(gfp_t gfp, const char *fmt, va_list args) { return kvasprintf(gfp, fmt, args); }

char *kasprintf(gfp_t gfp, const char *fmt, ...)
{
    va_list ap;
    char *s;
    va_start(ap, fmt);
    s = kvasprintf(gfp, fmt, ap);
    va_end(ap);
    return s;
}

int vprintk(const char *fmt, va_list args)
{
    char buf[512];
    int level = 6, n;
    if (fmt[0] == KERN_SOH[0] && fmt[1]) {
        level = fmt[1] >= '0' && fmt[1] <= '7' ? fmt[1] - '0' : 6;
        fmt += 2;
    }
    n = vscnprintf(buf, sizeof(buf), fmt, args);
    /* The level may also arrive through an argument ("%s%s", level, ...). */
    if (n >= 2 && buf[0] == KERN_SOH[0]) {
        level = buf[1] >= '0' && buf[1] <= '7' ? buf[1] - '0' : level;
        memmove(buf, buf + 2, n - 1);
        n -= 2;
    }
    while (n > 0 && buf[n - 1] == '\n')
        n--;
    if (n > 0)
        waveos_log(level, buf, n);
    return n;
}

int printk(const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vprintk(fmt, ap);
    va_end(ap);
    return n;
}

/* ---- Device messages ---- */
static int dev_vprintk(const char *level, const struct device *dev, const char *fmt, va_list ap)
{
    struct va_format vaf;
    va_list copy;
    int n;
    va_copy(copy, ap);
    vaf.fmt = fmt;
    vaf.va = &copy;
    if (level[0] == KERN_SOH[0] && level[1]) {
        char prefix[3] = { level[0], level[1], 0 };
        n = printk("%s%s: %pV", prefix, dev ? dev_name(dev) : "(no device)", &vaf);
    } else {
        n = printk("%s: %pV", dev ? dev_name(dev) : "(no device)", &vaf);
    }
    va_end(copy);
    return n;
}

int dev_printk(const char *level, const struct device *dev, const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = dev_vprintk(level, dev, fmt, ap);
    va_end(ap);
    return n;
}

int dev_err_probe(const struct device *dev, int err, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    if (err != -EPROBE_DEFER)
        dev_vprintk(KERN_ERR, dev, fmt, ap);
    va_end(ap);
    return err;
}

int dev_set_name(struct device *dev, const char *fmt, ...)
{
    va_list ap;
    char *name;
    va_start(ap, fmt);
    name = kvasprintf(GFP_KERNEL, fmt, ap);
    va_end(ap);
    if (!name)
        return -ENOMEM;
    kfree(dev->name);
    dev->name = name;
    return 0;
}

/* ---- seq_file and seq_buf ---- */
void seq_vprintf(struct seq_file *m, const char *fmt, va_list args)
{
    if (m->count < m->size) {
        int n = vsnprintf(m->buf + m->count, m->size - m->count, fmt, args);
        m->count = (size_t)n < m->size - m->count ? m->count + n : m->size;
    }
}

int seq_printf(struct seq_file *m, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    seq_vprintf(m, fmt, ap);
    va_end(ap);
    return 0;
}

int seq_buf_printf(struct seq_buf *s, const char *fmt, ...)
{
    va_list ap;
    int n = 0;
    va_start(ap, fmt);
    if (s->len < s->size) {
        n = vsnprintf(s->buffer + s->len, s->size - s->len, fmt, ap);
        s->len += n;
    } else {
        s->len = s->size + 1;
    }
    va_end(ap);
    return s->len > s->size ? -1 : 0;
}
