/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: files, user memory, seq_file and debugfs.
 *
 * WaveOS has no Linux user space, so the ported drivers are only driven
 * from inside the kernel (the compositor). These definitions let their
 * ioctl, mmap and debugfs code compile; "user" copies are plain copies of
 * kernel memory, and debugfs files are not created.
 */
#ifndef _WAVEOS_FS_H
#define _WAVEOS_FS_H

#include <waveos/misc.h>

struct inode {
    unsigned long i_ino;
    void *i_private;
    struct address_space *i_mapping;
    loff_t i_size;
};
struct address_space {
    struct inode *host;
    void *private_data;
    gfp_t gfp_mask;
};
struct path {
    void *mnt;
    void *dentry;
};
struct dentry {
    struct inode *d_inode;
};
struct poll_table_struct;
typedef struct poll_table_struct poll_table;
typedef unsigned int __poll_t;
#define EPOLLIN 0x0001
#define EPOLLRDNORM 0x0040
#define EPOLLOUT 0x0004
#define EPOLLERR 0x0008
#define EPOLLPRI 0x0002
#define EPOLLHUP 0x0010
#define poll_wait(f, wq, p) do { } while (0)
struct vm_area_struct;
struct file;
struct kiocb;
struct iov_iter;
struct file_operations {
    struct module *owner;
    loff_t (*llseek)(struct file *, loff_t, int);
    ssize_t (*read)(struct file *, char __user *, size_t, loff_t *);
    ssize_t (*write)(struct file *, const char __user *, size_t, loff_t *);
    __poll_t (*poll)(struct file *, struct poll_table_struct *);
    long (*unlocked_ioctl)(struct file *, unsigned int, unsigned long);
    long (*compat_ioctl)(struct file *, unsigned int, unsigned long);
    int (*mmap)(struct file *, struct vm_area_struct *);
    int (*open)(struct inode *, struct file *);
    int (*flush)(struct file *, void *);
    int (*release)(struct inode *, struct file *);
    int (*fasync)(int, struct file *, int);
    unsigned long (*get_unmapped_area)(struct file *, unsigned long, unsigned long, unsigned long, unsigned long);
    void (*show_fdinfo)(struct seq_file *m, struct file *f);
    unsigned int fop_flags;
};
struct file {
    const struct file_operations *f_op;
    struct inode *f_inode;
    struct address_space *f_mapping;
    void *private_data;
    unsigned int f_flags;
    fmode_t f_mode;
    loff_t f_pos;
    atomic_long_t f_count;
};
#define FOP_UNSIGNED_OFFSET 0x1
#define FMODE_READ 0x1
#define FMODE_WRITE 0x2
#define FMODE_UNSIGNED_OFFSET 0x2000
#define O_RDONLY 00000000
#define O_WRONLY 00000001
#define O_RDWR 00000002
#define O_CLOEXEC 02000000
#define O_NONBLOCK 00004000
#define O_EXCL 00000200
#define O_CREAT 00000100
#define file_inode(f) ((f)->f_inode)
#define iminor(inode) ((unsigned int)(inode)->i_ino)
#define imajor(inode) 0
#define register_chrdev(major, name, fops) 0
#define unregister_chrdev(major, name) do { } while (0)
#define noop_llseek NULL
#define no_llseek NULL
#define nonseekable_open(i, f) 0
#define stream_open(i, f) 0
struct file *fget(unsigned int fd);
void fput(struct file *f);
#define get_file(f) (f)
#define get_unused_fd_flags(flags) (-EMFILE)
#define put_unused_fd(fd) do { } while (0)
#define fd_install(fd, f) do { } while (0)
#define anon_inode_getfile(name, fops, priv, flags) ERR_PTR(-ENODEV)
/* Inodes for drivers' pseudo filesystems (DRM's address space for mmap offsets). */
struct super_block;
struct inode *alloc_anon_inode(struct super_block *sb);
void iput(struct inode *inode);
#define simple_read_from_buffer(to, count, ppos, from, available) (-EINVAL)
#define default_llseek NULL
#define unmap_mapping_range(m, s, l, e) do { } while (0)
#define fops_get(f) (f)
#define replace_fops(f, fops) do { (f)->f_op = (fops); } while (0)
#define fasync_helper(fd, f, on, fa) 0
#define kill_fasync(fp, sig, band) do { } while (0)
struct fasync_struct { int unused; };
struct super_block { int unused; };
struct vfsmount { struct super_block *mnt_sb; };
struct fs_context;
struct file_system_type {
    const char *name;
    struct module *owner;
    int (*init_fs_context)(struct fs_context *fc);
    void (*kill_sb)(struct super_block *sb);
};
#define kill_anon_super NULL
#define kern_mount(t) ERR_PTR(-ENODEV)
#define kern_unmount(m) do { } while (0)
int simple_pin_fs(struct file_system_type *type, struct vfsmount **mount, int *count);
void simple_release_fs(struct vfsmount **mount, int *count);
struct pseudo_fs_context { int unused; };
#define init_pseudo(fc, magic) ((struct pseudo_fs_context *)NULL)

/* "User" memory: WaveOS only calls these paths from the kernel. */
static inline unsigned long copy_from_user(void *to, const void __user *from, unsigned long n)
{ __builtin_memcpy(to, (const void *)from, n); return 0; }
static inline unsigned long copy_to_user(void __user *to, const void *from, unsigned long n)
{ __builtin_memcpy((void *)to, from, n); return 0; }
#define __copy_from_user copy_from_user
#define __copy_to_user copy_to_user
#define get_user(x, p) ({ (x) = *(p); 0; })
#define put_user(x, p) ({ *(p) = (x); 0; })
#define __get_user get_user
#define __put_user put_user
#define access_ok(p, n) 1
#define clear_user(p, n) ({ __builtin_memset((void *)(p), 0, n); 0UL; })
#define u64_to_user_ptr(x) ((void __user *)(uintptr_t)(x))
#define untagged_addr(x) (x)
#define strncpy_from_user(dst, src, n) ({ strscpy(dst, (const char *)(src), n); (long)strnlen(dst, n); })
#define copy_struct_from_user(dst, ksize, src, usize) ({ memcpy(dst, src, min(ksize, usize)); 0; })
#define user_access_begin(p, n) 1
#define user_access_end() do { } while (0)
#define unsafe_put_user(x, p, label) do { *(p) = (x); } while (0)
#define unsafe_get_user(x, p, label) do { (x) = *(p); } while (0)

/* seq_file: prints into a buffer the caller provides. */
struct seq_file {
    char *buf;
    size_t size;
    size_t count;
    void *private;
};
int seq_printf(struct seq_file *m, const char *fmt, ...) __printf(2, 3);
void seq_vprintf(struct seq_file *m, const char *fmt, va_list args);
static inline void seq_puts(struct seq_file *m, const char *s) { seq_printf(m, "%s", s); }
static inline void seq_putc(struct seq_file *m, char c) { seq_printf(m, "%c", c); }
#define seq_write(m, data, len) seq_printf(m, "%.*s", (int)(len), (const char *)(data))
#define seq_puts_ratelimited seq_puts
#define single_open(f, show, data) (-ENODEV)
#define single_release NULL
#define seq_read NULL
#define seq_lseek NULL
#define DEFINE_SHOW_ATTRIBUTE(name) static const struct file_operations name##_fops = { 0 }
struct seq_buf {
    char *buffer;
    size_t size;
    size_t len;
};

int seq_buf_printf(struct seq_buf *s, const char *fmt, ...) __printf(2, 3);
#define seq_buf_init(s, buf, size) do { (s)->buffer = (buf); (s)->size = (size); (s)->len = 0; } while (0)
#define DECLARE_SEQ_BUF(name, sz) char name##_buf[sz]; struct seq_buf name = { name##_buf, sz, 0 }
#define seq_buf_str(s) ((s)->buffer)
#define seq_buf_has_overflowed(s) ((s)->len > (s)->size)
#define seq_buf_used(s) min((s)->len, (s)->size)

/* debugfs: off. */
#define debugfs_create_dir(name, parent) ((struct dentry *)ERR_PTR(-ENODEV))
#define debugfs_create_file(name, mode, parent, data, fops) ((struct dentry *)ERR_PTR(-ENODEV))
#define debugfs_create_file_unsafe debugfs_create_file
#define debugfs_create_bool(name, mode, parent, v) do { } while (0)
#define debugfs_create_u32(name, mode, parent, v) do { } while (0)
#define debugfs_create_x32(name, mode, parent, v) do { } while (0)
#define debugfs_create_ulong(name, mode, parent, v) do { } while (0)
#define debugfs_create_size_t(name, mode, parent, v) do { } while (0)
#define debugfs_remove(d) do { } while (0)
#define debugfs_remove_recursive(d) do { } while (0)
#define debugfs_lookup_and_remove(n, p) do { } while (0)
#define debugfs_initialized() false
struct debugfs_reg32 { char *name; unsigned long offset; };
struct debugfs_regset32 {
    const struct debugfs_reg32 *regs;
    int nregs;
    void __iomem *base;
    struct device *dev;
};

#endif
