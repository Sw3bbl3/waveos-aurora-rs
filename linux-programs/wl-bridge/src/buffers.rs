//! The pixels programs attach to their surfaces: shared memory (`wl_shm`)
//! or dma-bufs (`zwp_linux_dmabuf_v1`, and Mesa's `wl_drm`, which is how
//! Mesa's EGL finds the DRM device to render on).
//!
//! A buffer reaches its window one of two ways. When a window shows one
//! surface and nothing else (no sub-surfaces or pop-ups with content, its
//! window geometry the whole buffer, no viewport scaling), the buffer is
//! attached to the WaveOS window as it is (`win_attach`): the compositor
//! reads it where it is — or a display scans it out — and WaveOS says when
//! it no longer reads it, which is when the program hears `wl_buffer.release`.
//! Otherwise the bridge copies the buffer and composes the window itself,
//! releasing the buffer at once, as it always did.

use crate::sys::{self, Fd};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const fn fourcc(s: &[u8; 4]) -> u32 {
    s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}

/// DRM fourcc codes (Wayland's `wl_shm` formats 0 and 1 are these two).
pub const XRGB8888: u32 = fourcc(b"XR24");
pub const ARGB8888: u32 = fourcc(b"AR24");
/// The layouts the bridge takes: linear, and "whatever the driver does
/// implicitly" (also linear here).
pub const MOD_LINEAR: u64 = 0;
pub const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// A `wl_shm` format as a fourcc.
pub fn shm_format(f: u32) -> u32 {
    match f {
        0 => ARGB8888,
        1 => XRGB8888,
        other => other,
    }
}

pub struct Pool {
    pub fd: Fd,
    pub ptr: *mut u8,
    pub size: usize,
}

impl Pool {
    pub fn map(fd: Fd, size: usize) -> Option<Pool> {
        let ptr = sys::mmap_shared(fd, size.max(1))?;
        Some(Pool { fd, ptr, size })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        sys::munmap(self.ptr, self.size.max(1));
        sys::close(self.fd);
    }
}

pub enum Memory {
    Shm(Rc<RefCell<Pool>>),
    /// A dma-buf, mapped (for copying) the first time it is copied.
    Dma { fd: Fd, map: Cell<Option<(*mut u8, usize)>> },
}

pub struct Buffer {
    pub memory: Memory,
    /// Bytes from the start of the pool or dma-buf to the first pixel.
    pub offset: usize,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    /// DRM fourcc.
    pub format: u32,
    /// The WaveOS buffer (`win_buffer`) once the buffer was attached to a
    /// window as it is; 0 if it can't be (WaveOS refused it).
    pub kernel: Cell<Option<u32>>,
}

impl Buffer {
    pub fn shm(pool: Rc<RefCell<Pool>>, offset: usize, width: i32, height: i32, stride: i32, format: u32) -> Buffer {
        Buffer {
            memory: Memory::Shm(pool),
            offset,
            width,
            height,
            stride,
            format: shm_format(format),
            kernel: Cell::new(None),
        }
    }

    pub fn dma(fd: Fd, offset: usize, width: i32, height: i32, stride: i32, format: u32) -> Buffer {
        Buffer {
            memory: Memory::Dma { fd, map: Cell::new(None) },
            offset,
            width,
            height,
            stride,
            format,
            kernel: Cell::new(None),
        }
    }

    pub fn opaque(&self) -> bool {
        self.format == XRGB8888
    }

    /// A format the bridge can show.
    pub fn supported(format: u32) -> bool {
        format == XRGB8888 || format == ARGB8888
    }

    /// The buffer's bytes from its first pixel (pointer, bytes there), for
    /// copying; a dma-buf is mapped on first use.
    pub fn bytes(&self) -> Option<(*const u8, usize)> {
        match &self.memory {
            Memory::Shm(pool) => {
                let p = pool.borrow();
                (self.offset <= p.size).then(|| (unsafe { p.ptr.add(self.offset) } as *const u8, p.size - self.offset))
            }
            Memory::Dma { fd, map } => {
                let (ptr, size) = match map.get() {
                    Some(m) => m,
                    None => {
                        let size = self.offset + self.stride.max(0) as usize * self.height.max(0) as usize;
                        let ptr = sys::mmap_shared(*fd, size.max(1))?;
                        map.set(Some((ptr, size)));
                        (ptr, size)
                    }
                };
                (self.offset <= size).then(|| (unsafe { ptr.add(self.offset) } as *const u8, size - self.offset))
            }
        }
    }

    /// The WaveOS buffer for attaching this one to a window as it is,
    /// made on first use; `None` if WaveOS can't show it.
    pub fn kernel_id(&self) -> Option<u32> {
        if let Some(id) = self.kernel.get() {
            return (id != 0).then_some(id);
        }
        let fd = match &self.memory {
            Memory::Shm(pool) => pool.borrow().fd,
            Memory::Dma { fd, .. } => *fd,
        };
        let id = sys::win_buffer(fd, self.offset, self.width, self.height, self.stride, self.format).unwrap_or(0);
        self.kernel.set(Some(id));
        (id != 0).then_some(id)
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if let Some(id) = self.kernel.get().filter(|&id| id != 0) {
            // WaveOS keeps it while a window still shows it.
            sys::win_buffer_destroy(id);
        }
        if let Memory::Dma { fd, map } = &self.memory {
            if let Some((ptr, size)) = map.get() {
                sys::munmap(ptr, size.max(1));
            }
            sys::close(*fd);
        }
    }
}
