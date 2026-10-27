//! The Linux driver compatibility layer (LinuxKPI): the Rust half.
//!
//! `kernel/linux/` holds a C implementation of the Linux kernel interfaces
//! (written for WaveOS, MIT) and Linux drivers compiled against it — today
//! the DRM core and the virtio-gpu driver. This module supplies what that C
//! code needs from the kernel (memory, time, logging, a worker thread for
//! deferred work), finds devices for the drivers and hands the compositor
//! the display they drive.
//!
//! Everything here is built only when the C half was (`cfg(linuxkpi)`; it
//! needs clang at build time).

#[cfg(linuxkpi)]
mod virtio;

#[cfg(linuxkpi)]
pub use imp::*;

#[cfg(linuxkpi)]
mod imp {
    use super::virtio;
    use crate::mm::{self, frame, vmm, PAGE_SIZE};
    use crate::sched::{self, TaskId};
    use crate::sync::{IrqMutex, WaitQueue};
    use alloc::alloc::{alloc, dealloc, Layout};
    use core::ffi::{c_char, c_int, c_void};
    use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

    extern "C" {
        fn waveos_init_drm_core_init() -> c_int;
        fn waveos_lkpi_run_pending() -> u64;
        fn waveos_drm_display_open(dev: *mut c_void, max_w: u32, max_h: u32, info: *mut DisplayInfo) -> *mut c_void;
        fn waveos_drm_display_flush(display: *mut c_void, x: c_int, y: c_int, w: c_int, h: c_int) -> c_int;
    }

    /// `struct waveos_display_info` in kernel/linux/drm/waveos_display.c.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct DisplayInfo {
        pub pixels: *mut u32,
        pub width: u32,
        pub height: u32,
        pub pitch: u32,
    }

    // The pixels are kernel memory that lives as long as the display.
    unsafe impl Send for DisplayInfo {}

    // ------------------------------------------------------------- memory

    const HEAP_ALIGN: usize = 16;

    #[no_mangle]
    extern "C" fn waveos_heap_alloc(size: usize) -> *mut u8 {
        match Layout::from_size_align(size.max(1), HEAP_ALIGN) {
            Ok(l) => unsafe { alloc(l) },
            Err(_) => core::ptr::null_mut(),
        }
    }

    #[no_mangle]
    extern "C" fn waveos_heap_free(p: *mut u8, size: usize) {
        if let Ok(l) = Layout::from_size_align(size.max(1), HEAP_ALIGN) {
            unsafe { dealloc(p, l) }
        }
    }

    #[no_mangle]
    extern "C" fn waveos_frame_alloc() -> u64 {
        frame::alloc().unwrap_or(0)
    }

    #[no_mangle]
    extern "C" fn waveos_frame_free(phys: u64) {
        frame::free(phys)
    }

    #[no_mangle]
    extern "C" fn waveos_frames_alloc_contiguous(count: usize) -> u64 {
        frame::alloc_contiguous(count as u64).unwrap_or(0)
    }

    #[no_mangle]
    extern "C" fn waveos_phys_to_virt(phys: u64) -> *mut c_void {
        mm::phys_to_virt(phys) as *mut c_void
    }

    #[no_mangle]
    extern "C" fn waveos_virt_to_phys(virt: *const c_void) -> u64 {
        let va = virt as u64;
        // The physical window is the common case (the heap lives there).
        if (bootinfo::PHYS_OFFSET..bootinfo::PHYS_OFFSET + (1 << 46)).contains(&va) {
            return va - bootinfo::PHYS_OFFSET;
        }
        vmm::translate(va).unwrap_or(0)
    }

    #[no_mangle]
    extern "C" fn waveos_kmap_frames(frames: *const u64, count: usize) -> *mut c_void {
        let frames = unsafe { core::slice::from_raw_parts(frames, count) };
        vmm::kmap(frames).map_or(core::ptr::null_mut(), |va| va as *mut c_void)
    }

    #[no_mangle]
    extern "C" fn waveos_kunmap(virt: *mut c_void, count: usize) {
        vmm::kunmap(virt as u64, count as u64)
    }

    #[no_mangle]
    extern "C" fn waveos_map_mmio(phys: u64, size: u64, write_combining: bool) -> *mut c_void {
        let va = if write_combining { vmm::kmap_wc(phys, size) } else { None };
        va.unwrap_or_else(|| mm::paging::map_mmio(phys, size.max(PAGE_SIZE))) as *mut c_void
    }

    // ------------------------------------------------------ time, logging

    #[no_mangle]
    extern "C" fn waveos_now_ns() -> u64 {
        crate::time::now_ns()
    }

    #[no_mangle]
    extern "C" fn waveos_yield() {
        sched::yield_now()
    }

    #[no_mangle]
    extern "C" fn waveos_sleep_us(us: u64) {
        let end = crate::time::now_ns() + us * 1000;
        if us >= 2000 {
            sched::sleep_ms(us / 1000);
        }
        while crate::time::now_ns() < end {
            core::hint::spin_loop();
        }
    }

    #[no_mangle]
    extern "C" fn waveos_log(level: c_int, msg: *const c_char, len: usize) {
        let bytes = unsafe { core::slice::from_raw_parts(msg as *const u8, len) };
        let text = core::str::from_utf8(bytes).unwrap_or("(unprintable message)");
        // Linux debug messages (level 7) are noise in the WaveOS log.
        if level < 7 {
            log!("linux", "{}", text);
        }
    }

    /// Logs the return addresses on the stack (the kernel keeps frame pointers).
    #[no_mangle]
    extern "C" fn waveos_backtrace() {
        let mut rbp: u64;
        unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp) };
        for depth in 0..16 {
            // Frames live on kernel stacks (heap); stop at anything else.
            if rbp & 7 != 0 || rbp < bootinfo::PHYS_OFFSET {
                break;
            }
            let (next, ret) = unsafe { (*(rbp as *const u64), *((rbp + 8) as *const u64)) };
            if ret < 0xFFFF_FFFF_8000_0000 {
                break;
            }
            log!("linux", "  #{depth} {ret:#x}");
            if next <= rbp {
                break;
            }
            rbp = next;
        }
    }

    #[no_mangle]
    extern "C" fn waveos_panic(msg: *const c_char, len: usize) -> ! {
        let bytes = unsafe { core::slice::from_raw_parts(msg as *const u8, len) };
        panic!("{}", core::str::from_utf8(bytes).unwrap_or("Linux driver BUG"));
    }

    // ------------------------------------------------------------ the worker

    static WORKER: AtomicU64 = AtomicU64::new(u64::MAX);
    static KICKED: AtomicBool = AtomicBool::new(false);
    static WAKE: WaitQueue = WaitQueue::new();

    #[no_mangle]
    extern "C" fn waveos_is_worker() -> bool {
        WORKER.load(Ordering::Relaxed) == sched::current_id() as u64
    }

    /// Wakes the worker (from any context, interrupts included).
    #[no_mangle]
    pub extern "C" fn waveos_worker_kick() {
        KICKED.store(true, Ordering::Release);
        WAKE.wake_all();
    }

    #[no_mangle]
    extern "C" fn waveos_poll_devices() {
        virtio::poll();
    }

    /// "linux-worker": runs the drivers' deferred work and timers, and
    /// delivers virtqueue completions.
    fn worker() {
        loop {
            KICKED.store(false, Ordering::Release);
            virtio::poll();
            let next = unsafe { waveos_lkpi_run_pending() };
            let now = crate::time::now_ns();
            // Devices without interrupts are polled every few milliseconds.
            let mut ms = if virtio::needs_polling() { 4 } else { 1000 };
            if next != 0 {
                ms = ms.min((next.saturating_sub(now) / 1_000_000).max(1));
            }
            WAKE.wait(ms, || KICKED.load(Ordering::Acquire));
        }
    }

    // --------------------------------------------------------------- startup

    struct Display {
        handle: AtomicPtr<c_void>,
        info: IrqMutex<Option<DisplayInfo>>,
    }

    static DISPLAY: Display = Display { handle: AtomicPtr::new(core::ptr::null_mut()), info: IrqMutex::new(None) };

    /// Starts the worker, initializes DRM and probes the GPUs the ported
    /// drivers support. Call after PCI enumeration, with the scheduler running.
    pub fn init() {
        let id: TaskId = sched::spawn("linux-worker", worker);
        WORKER.store(id as u64, Ordering::Relaxed);
        let ret = unsafe { waveos_init_drm_core_init() };
        if ret != 0 {
            log!("linux", "DRM core failed to start ({ret})");
            return;
        }
        for drm in virtio::probe_gpus() {
            let mut info = DisplayInfo { pixels: core::ptr::null_mut(), width: 0, height: 0, pitch: 0 };
            let handle = unsafe { waveos_drm_display_open(drm, 0, 0, &mut info) };
            if (handle as isize) < 0 && (handle as isize) > -4096 {
                log!("linux", "no display on the GPU ({})", handle as isize);
                continue;
            }
            DISPLAY.handle.store(handle, Ordering::Release);
            *DISPLAY.info.lock() = Some(info);
            break;
        }
    }

    /// The display a ported GPU driver runs, if there is one.
    pub fn display() -> Option<DisplayInfo> {
        *DISPLAY.info.lock()
    }

    /// Shows the pixels in the rectangle (after drawing into `display().pixels`).
    pub fn display_flush(x: i32, y: i32, w: i32, h: i32) -> bool {
        let handle = DISPLAY.handle.load(Ordering::Acquire);
        !handle.is_null() && unsafe { waveos_drm_display_flush(handle, x, y, w, h) } == 0
    }
}

/// Without the C half there is nothing to start.
#[cfg(not(linuxkpi))]
pub fn init() {}
