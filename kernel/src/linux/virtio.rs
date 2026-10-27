//! virtio for the ported Linux drivers: the transport and rings are
//! WaveOS's own (`drivers::virtio`); this module finds the devices, creates
//! their Linux-side objects and backs the virtqueue calls the C code makes.

use crate::drivers::pci::{self, Bar};
use crate::drivers::virtio::{Transport, Virtqueue, F_VERSION_1};
use crate::sync::IrqMutex;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::{c_int, c_uint, c_void};
use core::sync::atomic::{AtomicBool, Ordering};

const VIRTIO_ID_GPU: u32 = 16;
/// Ring size: enough for virtio-gpu's command queue and cursor queue.
const MAX_QUEUE: u16 = 256;
const ENOSPC: c_int = 28;

extern "C" {
    static waveos_virtio_driver_virtio_gpu_driver: *const c_void;
    fn waveos_virtio_driver_features(drv: *const c_void) -> u64;
    fn waveos_pci_dev_create(
        vendor: u16,
        device: u16,
        class: u32,
        bus: u8,
        devfn: u8,
        bar_start: *const u64,
        bar_len: *const u64,
    ) -> *mut c_void;
    fn waveos_virtio_device_create(rust: *mut c_void, id: u32, features: u64, parent: *mut c_void) -> *mut c_void;
    fn waveos_virtio_probe(vdev: *mut c_void, drv: *const c_void) -> c_int;
    fn waveos_virtio_drvdata(vdev: *mut c_void) -> *mut c_void;
    fn waveos_virtio_vq_interrupt(vq: *mut c_void);
}

struct Ring {
    q: Virtqueue,
    /// The driver's token for each chain, by head descriptor.
    tokens: Vec<usize>,
}

struct Queue {
    index: u16,
    ring: IrqMutex<Ring>,
    /// The Linux `struct virtqueue`.
    linux: usize,
}

struct Device {
    t: Transport,
    msix: bool,
    /// Some queue did not take the interrupt vector: poll.
    polling: AtomicBool,
    queues: IrqMutex<Vec<&'static Queue>>,
}

static DEVICES: IrqMutex<Vec<&'static Device>> = IrqMutex::new(Vec::new());

fn interrupt(_: usize) {
    super::waveos_worker_kick();
}

/// Probes every virtio-gpu (and virtio-vga) with the Linux driver; returns
/// the `struct drm_device` of each that came up.
pub fn probe_gpus() -> Vec<*mut c_void> {
    let mut out = Vec::new();
    for d in pci::devices().into_iter().filter(|d| d.vendor == 0x1AF4 && d.device == 0x1040 + VIRTIO_ID_GPU as u16) {
        d.enable();
        let Some(t) = Transport::probe(&d) else { continue };
        let drv = unsafe { waveos_virtio_driver_virtio_gpu_driver };
        let wanted = unsafe { waveos_virtio_driver_features(drv) } | F_VERSION_1;
        let Some(features) = t.negotiate(wanted, F_VERSION_1) else {
            log!("virtio-gpu", "{:02x}:{:02x}.{}: feature negotiation failed", d.bus, d.dev, d.func);
            continue;
        };
        let msix = d.enable_msi("virtio-gpu", interrupt, 0).is_some();
        t.no_config_interrupt();
        let dev: &'static Device = Box::leak(Box::new(Device {
            t,
            msix,
            polling: AtomicBool::new(!msix),
            queues: IrqMutex::new(Vec::new()),
        }));
        DEVICES.lock().push(dev);

        let (mut start, mut len) = ([0u64; 6], [0u64; 6]);
        for (i, bar) in d.bars.iter().enumerate() {
            if let Bar::Memory { phys, size } = *bar {
                (start[i], len[i]) = (phys, size);
            }
        }
        let class = (d.class as u32) << 16 | (d.subclass as u32) << 8 | d.prog_if as u32;
        let devfn = d.dev << 3 | d.func;
        let pdev =
            unsafe { waveos_pci_dev_create(d.vendor, d.device, class, d.bus, devfn, start.as_ptr(), len.as_ptr()) };
        if pdev.is_null() {
            continue;
        }
        let vdev =
            unsafe { waveos_virtio_device_create(dev as *const Device as *mut c_void, VIRTIO_ID_GPU, features, pdev) };
        if vdev.is_null() {
            continue;
        }
        let ret = unsafe { waveos_virtio_probe(vdev, drv) };
        if ret != 0 {
            log!("virtio-gpu", "the Linux driver did not take the device ({ret})");
            continue;
        }
        log!("virtio-gpu", "Linux virtio-gpu driver bound{}", if msix { ", interrupts via MSI-X" } else { "" });
        out.push(unsafe { waveos_virtio_drvdata(vdev) });
    }
    out
}

/// Whether some device has to be polled for completions.
pub fn needs_polling() -> bool {
    DEVICES.lock().iter().any(|d| d.polling.load(Ordering::Relaxed))
}

/// Delivers completions: calls the driver's callback for every queue with
/// used buffers waiting (no locks held during the calls).
pub fn poll() {
    let mut ready: Vec<usize> = Vec::new();
    for dev in DEVICES.lock().iter() {
        for q in dev.queues.lock().iter() {
            if q.ring.lock().q.has_used() {
                ready.push(q.linux);
            }
        }
    }
    for vq in ready {
        unsafe { waveos_virtio_vq_interrupt(vq as *mut c_void) };
    }
}

fn device(handle: *mut c_void) -> &'static Device {
    unsafe { &*(handle as *const Device) }
}

fn queue(handle: *mut c_void) -> &'static Queue {
    unsafe { &*(handle as *const Queue) }
}

/// Device configuration access with the field's natural width.
#[no_mangle]
extern "C" fn waveos_virtio_config_read(dev: *mut c_void, offset: c_uint, buf: *mut u8, len: c_uint) {
    let base = device(dev).t.device + offset as u64;
    unsafe {
        match len {
            1 => *buf = (base as *const u8).read_volatile(),
            2 => (buf as *mut u16).write_unaligned((base as *const u16).read_volatile()),
            4 => (buf as *mut u32).write_unaligned((base as *const u32).read_volatile()),
            8 => {
                (buf as *mut u32).write_unaligned((base as *const u32).read_volatile());
                (buf.add(4) as *mut u32).write_unaligned(((base + 4) as *const u32).read_volatile());
            }
            n => {
                for i in 0..n as usize {
                    *buf.add(i) = ((base + i as u64) as *const u8).read_volatile();
                }
            }
        }
    }
}

#[no_mangle]
extern "C" fn waveos_virtio_config_write(dev: *mut c_void, offset: c_uint, buf: *const u8, len: c_uint) {
    let base = device(dev).t.device + offset as u64;
    unsafe {
        match len {
            1 => (base as *mut u8).write_volatile(*buf),
            2 => (base as *mut u16).write_volatile((buf as *const u16).read_unaligned()),
            4 => (base as *mut u32).write_volatile((buf as *const u32).read_unaligned()),
            n => {
                for i in 0..n as usize {
                    ((base + i as u64) as *mut u8).write_volatile(*buf.add(i));
                }
            }
        }
    }
}

#[no_mangle]
extern "C" fn waveos_virtio_queue(dev: *mut c_void, index: c_uint, linux_vq: *mut c_void) -> *mut c_void {
    let dev = device(dev);
    let index = index as u16;
    let max = dev.t.queue_max(index);
    if max == 0 {
        return core::ptr::null_mut();
    }
    let size = max.min(MAX_QUEUE);
    let Some(mut q) = Virtqueue::new(size) else { return core::ptr::null_mut() };
    let irq = dev.t.attach_queue(index, &mut q, dev.msix.then_some(0));
    if !irq {
        dev.polling.store(true, Ordering::Relaxed);
    }
    let queue: &'static Queue = Box::leak(Box::new(Queue {
        index,
        ring: IrqMutex::new(Ring { q, tokens: vec![0; size as usize] }),
        linux: linux_vq as usize,
    }));
    dev.queues.lock().push(queue);
    queue as *const Queue as *mut c_void
}

#[no_mangle]
extern "C" fn waveos_virtio_ready(dev: *mut c_void) {
    device(dev).t.driver_ok();
}

#[no_mangle]
extern "C" fn waveos_virtio_reset(dev: *mut c_void) {
    device(dev).t.reset();
}

/// Shared-memory regions (host-visible VRAM) are not offered.
#[no_mangle]
extern "C" fn waveos_virtio_shm_region(_dev: *mut c_void, _addr: *mut u64, _len: *mut u64, _id: u8) -> bool {
    false
}

#[no_mangle]
extern "C" fn waveos_vq_add(
    vq: *mut c_void,
    addr: *const u64,
    len: *const u32,
    write: *const u8,
    n: c_uint,
    token: *mut c_void,
) -> c_int {
    let n = n as usize;
    let (addr, len, write) = unsafe {
        (
            core::slice::from_raw_parts(addr, n),
            core::slice::from_raw_parts(len, n),
            core::slice::from_raw_parts(write, n),
        )
    };
    let bufs: Vec<(u64, u32, bool)> = (0..n).map(|i| (addr[i], len[i], write[i] != 0)).collect();
    let mut ring = queue(vq).ring.lock();
    match ring.q.add(&bufs) {
        Some(head) => {
            ring.tokens[head as usize] = token as usize;
            0
        }
        None => -ENOSPC,
    }
}

#[no_mangle]
extern "C" fn waveos_vq_kick(vq: *mut c_void) {
    let q = queue(vq);
    q.ring.lock().q.notify(q.index);
}

#[no_mangle]
extern "C" fn waveos_vq_get_buf(vq: *mut c_void, len: *mut c_uint) -> *mut c_void {
    let mut ring = queue(vq).ring.lock();
    match ring.q.pop_used() {
        Some((head, written)) => {
            if !len.is_null() {
                unsafe { *len = written };
            }
            core::mem::take(&mut ring.tokens[head as usize]) as *mut c_void
        }
        None => core::ptr::null_mut(),
    }
}

#[no_mangle]
extern "C" fn waveos_vq_num_free(vq: *mut c_void) -> c_uint {
    queue(vq).ring.lock().q.num_free() as c_uint
}

#[no_mangle]
extern "C" fn waveos_vq_has_used(vq: *mut c_void) -> bool {
    queue(vq).ring.lock().q.has_used()
}
