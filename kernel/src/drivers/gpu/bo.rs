//! Buffer objects and framebuffers.
//!
//! A [`BufferObject`] is pixel memory, refcounted (`Arc`), like a GEM
//! object: system RAM in physically contiguous runs (as large as the
//! frame allocator gives), mapped once into the kernel. The runs are the
//! scatter list a device DMAs from; the mapping is what the CPU draws
//! into. A device driver may bind its own object to it (virtio-gpu: a host
//! resource) through [`BufferObject::bind`]; the binding is released
//! before the memory is freed, when the last reference goes.
//!
//! A [`Framebuffer`] views a buffer object as an image (size, pitch,
//! format). Planes scan framebuffers out, so a client buffer can be shown
//! without a copy once it is wrapped in one.

use super::{new_id, Error, ObjId, Result};
use crate::mm::{frame, vmm, PAGE_SIZE};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

/// DRM fourcc pixel formats (little-endian words).
pub mod fourcc {
    const fn code(s: &[u8; 4]) -> u32 {
        s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
    }
    /// 0xXXRRGGBB words: the compositor's own layout.
    pub const XRGB8888: u32 = code(b"XR24");
    /// 0xAARRGGBB words, straight alpha (cursors).
    pub const ARGB8888: u32 = code(b"AR24");
}

/// What a driver does when a buffer it bound goes away.
pub trait Unbind: Send + Sync {
    fn unbind(&self, handle: u32);
}

pub struct BufferObject {
    pub id: ObjId,
    pub size: usize,
    /// Physically contiguous runs: (physical address, bytes).
    chunks: Vec<(u64, usize)>,
    vaddr: u64,
    pages: u64,
    /// The binding a driver made (e.g. a virtio-gpu resource id), 0 if none.
    handle: AtomicU32,
    owner: spin::Mutex<Option<Arc<dyn Unbind>>>,
}

impl BufferObject {
    /// Zeroed system memory of at least `size` bytes, mapped into the kernel.
    pub fn alloc_system(size: usize) -> Result<Arc<BufferObject>> {
        let pages = (size as u64).div_ceil(PAGE_SIZE).max(1);
        let mut chunks: Vec<(u64, usize)> = Vec::new();
        let mut frames: Vec<u64> = Vec::with_capacity(pages as usize);
        let mut run = pages.min(2048);
        let mut left = pages;
        let free_all = |chunks: &[(u64, usize)]| {
            for &(phys, bytes) in chunks {
                for p in 0..bytes as u64 / PAGE_SIZE {
                    frame::free(phys + p * PAGE_SIZE);
                }
            }
        };
        while left > 0 {
            let n = run.min(left);
            match frame::alloc_contiguous(n) {
                Some(phys) => {
                    // Merge with the previous run when it happens to follow on.
                    match chunks.last_mut() {
                        Some(last) if last.0 + last.1 as u64 == phys => last.1 += (n * PAGE_SIZE) as usize,
                        _ => chunks.push((phys, (n * PAGE_SIZE) as usize)),
                    }
                    frames.extend((0..n).map(|i| phys + i * PAGE_SIZE));
                    left -= n;
                }
                None if run > 1 => run /= 2,
                None => {
                    free_all(&chunks);
                    return Err(Error::NoMemory);
                }
            }
        }
        let Some(vaddr) = vmm::kmap(&frames) else {
            free_all(&chunks);
            return Err(Error::NoMemory);
        };
        unsafe { core::ptr::write_bytes(vaddr as *mut u8, 0, (pages * PAGE_SIZE) as usize) };
        Ok(Arc::new(BufferObject {
            id: new_id(),
            size: (pages * PAGE_SIZE) as usize,
            chunks,
            vaddr,
            pages,
            handle: AtomicU32::new(0),
            owner: spin::Mutex::new(None),
        }))
    }

    /// The kernel address of the first byte.
    pub fn vaddr(&self) -> u64 {
        self.vaddr
    }

    /// The scatter list: (physical address, bytes) runs, in order.
    pub fn chunks(&self) -> &[(u64, usize)] {
        &self.chunks
    }

    /// The driver binding, 0 if none.
    pub fn handle(&self) -> u32 {
        self.handle.load(Ordering::Acquire)
    }

    /// Records a driver binding; `owner.unbind(handle)` runs when the
    /// buffer is freed. One binding per buffer.
    pub fn bind(&self, handle: u32, owner: Arc<dyn Unbind>) {
        *self.owner.lock() = Some(owner);
        self.handle.store(handle, Ordering::Release);
    }
}

impl Drop for BufferObject {
    fn drop(&mut self) {
        // The device lets go of the memory before it is reused.
        let handle = self.handle.swap(0, Ordering::AcqRel);
        if let Some(owner) = self.owner.lock().take() {
            if handle != 0 {
                owner.unbind(handle);
            }
        }
        vmm::kunmap(self.vaddr, self.pages);
        for &(phys, bytes) in &self.chunks {
            for p in 0..bytes as u64 / PAGE_SIZE {
                frame::free(phys + p * PAGE_SIZE);
            }
        }
    }
}

/// An image in a buffer object.
pub struct Framebuffer {
    pub id: ObjId,
    pub bo: Arc<BufferObject>,
    pub width: u32,
    pub height: u32,
    /// Bytes from one row to the next.
    pub pitch: u32,
    /// Byte offset of the first pixel in the buffer.
    pub offset: u32,
    pub format: u32,
}

impl core::fmt::Debug for Framebuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Framebuffer #{} {}x{} pitch {} format {:#x}",
            self.id, self.width, self.height, self.pitch, self.format
        )
    }
}

impl Framebuffer {
    pub fn new(bo: Arc<BufferObject>, width: u32, height: u32, pitch: u32, format: u32) -> Result<Arc<Framebuffer>> {
        if width == 0 || height == 0 || pitch < width * 4 || !pitch.is_multiple_of(4) {
            return Err(Error::Invalid);
        }
        if pitch as u64 * height as u64 > bo.size as u64 {
            return Err(Error::Invalid);
        }
        Ok(Arc::new(Framebuffer { id: new_id(), bo, width, height, pitch, offset: 0, format }))
    }

    /// A `w`×`h` 32-bit image in fresh system memory, rows packed.
    pub fn new_system(w: u32, h: u32, format: u32) -> Result<Arc<Framebuffer>> {
        if w == 0 || h == 0 || w > 16384 || h > 16384 {
            return Err(Error::Invalid);
        }
        let bo = BufferObject::alloc_system(w as usize * h as usize * 4)?;
        Framebuffer::new(bo, w, h, w * 4, format)
    }

    /// The pixels as 32-bit words, `pitch / 4` per row.
    ///
    /// # Safety
    /// The caller must be the only writer (the compositor for its back
    /// buffer), and must not keep the slice past the framebuffer's life.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn pixels(&self) -> &mut [u32] {
        let words = (self.pitch / 4) as usize * self.height as usize;
        core::slice::from_raw_parts_mut((self.bo.vaddr() + self.offset as u64) as *mut u32, words)
    }

    /// Read-only view of the pixels.
    pub fn read_pixels(&self) -> &[u32] {
        let words = (self.pitch / 4) as usize * self.height as usize;
        unsafe { core::slice::from_raw_parts((self.bo.vaddr() + self.offset as u64) as *const u32, words) }
    }
}
