//! virtio-net: a receive queue kept full of 2 KiB buffers and a transmit
//! queue of copied frames. Received frames are handed to the network stack
//! from the MSI-X handler (or polled without one).

use crate::drivers::block::Dma;
use crate::drivers::pci;
use crate::drivers::virtio::{device_bytes, Transport, Virtqueue, F_VERSION_1};
use crate::net::{self, Nic};
use crate::sync::IrqMutex;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const F_MAC: u64 = 1 << 5;
const F_STATUS: u64 = 1 << 16;
/// With VERSION_1 the header always carries `num_buffers`: 12 bytes.
const HDR: usize = 12;
const BUF: usize = 2048;
const QUEUE: u16 = 128;

struct Ring {
    q: Virtqueue,
    bufs: Dma,
    /// Descriptor head → buffer slot.
    slot_of: Vec<Option<usize>>,
    free_slots: Vec<usize>,
}

impl Ring {
    fn new(size: u16) -> Option<Ring> {
        Some(Ring {
            q: Virtqueue::new(size)?,
            bufs: Dma::new(size as usize * BUF)?,
            slot_of: vec![None; size as usize],
            free_slots: (0..size as usize).rev().collect(),
        })
    }
}

pub struct VirtioNet {
    t: Transport,
    features: u64,
    mac: [u8; 6],
    has_status: bool,
    rx: IrqMutex<Ring>,
    tx: IrqMutex<Ring>,
    iface: AtomicUsize,
    has_irq: AtomicBool,
}

impl VirtioNet {
    /// Gives the device an empty buffer to receive into.
    fn post_rx(r: &mut Ring) {
        while let Some(slot) = r.free_slots.pop() {
            let phys = r.bufs.phys + (slot * BUF) as u64;
            match r.q.add(&[(phys, BUF as u32, true)]) {
                Some(head) => r.slot_of[head as usize] = Some(slot),
                None => {
                    r.free_slots.push(slot);
                    break;
                }
            }
        }
    }

    /// Hands every received frame to the stack and refills the queue.
    fn collect(&self) {
        let mut frames = Vec::new();
        {
            let mut r = self.rx.lock();
            while let Some((head, len)) = r.q.pop_used() {
                let Some(slot) = r.slot_of[head as usize].take() else { continue };
                let len = len as usize;
                if len > HDR && len <= BUF {
                    let start = slot * BUF;
                    frames.push(r.bufs.bytes_mut()[start + HDR..start + len].to_vec());
                }
                r.free_slots.push(slot);
            }
            Self::post_rx(&mut r);
            r.q.notify(0);
        }
        let iface = self.iface.load(Ordering::Acquire);
        for f in frames {
            net::receive(iface, f);
        }
    }
}

fn interrupt(arg: usize) {
    let n = unsafe { &*(arg as *const VirtioNet) };
    n.collect();
}

impl Nic for VirtioNet {
    fn driver(&self) -> String {
        String::from("virtio-net")
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&self) -> bool {
        !self.has_status || device_bytes(&self.t, 6, 1)[0] & 1 != 0
    }
    fn send(&self, frame: &[u8]) -> bool {
        if frame.len() + HDR > BUF {
            return false;
        }
        let mut t = self.tx.lock();
        // Reclaim buffers the device has sent.
        while let Some((head, _)) = t.q.pop_used() {
            if let Some(slot) = t.slot_of[head as usize].take() {
                t.free_slots.push(slot);
            }
        }
        let Some(slot) = t.free_slots.pop() else { return false };
        let start = slot * BUF;
        let buf = &mut t.bufs.bytes_mut()[start..start + HDR + frame.len()];
        buf[..HDR].fill(0);
        buf[HDR..].copy_from_slice(frame);
        let phys = t.bufs.phys + start as u64;
        match t.q.add(&[(phys, (HDR + frame.len()) as u32, false)]) {
            Some(head) => {
                t.slot_of[head as usize] = Some(slot);
                t.q.notify(1);
                true
            }
            None => {
                t.free_slots.push(slot);
                false
            }
        }
    }
    fn poll(&self) {
        if !self.has_irq.load(Ordering::Relaxed) || self.rx.lock().q.has_used() {
            self.collect();
        }
    }
}

pub fn probe(dev: &pci::Device) {
    dev.enable();
    let Some(t) = Transport::probe(dev) else {
        log!("virtio-net", "device {:04x} lacks modern virtio capabilities", dev.device);
        return;
    };
    let Some(features) = t.negotiate(F_VERSION_1 | F_MAC | F_STATUS, F_VERSION_1) else {
        log!("virtio-net", "feature negotiation failed");
        return;
    };
    let size = |i: u16| t.queue_max(i).min(QUEUE);
    let (Some(rx), Some(tx)) = (Ring::new(size(0)), Ring::new(size(1))) else { return };
    let mac: [u8; 6] = if features & F_MAC != 0 {
        device_bytes(&t, 0, 6).try_into().unwrap()
    } else {
        // Locally administered, random.
        let r = crate::random::u64().to_le_bytes();
        [0x02, r[0], r[1], r[2], r[3], r[4]]
    };
    let nic: &'static VirtioNet = Box::leak(Box::new(VirtioNet {
        t,
        features,
        mac,
        has_status: features & F_STATUS != 0,
        rx: IrqMutex::new(rx),
        tx: IrqMutex::new(tx),
        iface: AtomicUsize::new(0),
        has_irq: AtomicBool::new(false),
    }));
    let msix = dev.enable_msi("virtio-net", interrupt, nic as *const VirtioNet as usize).is_some();
    t.no_config_interrupt();
    let rx_irq = t.attach_queue(0, &mut nic.rx.lock().q, msix.then_some(0));
    // Transmit completions are reclaimed lazily: no interrupt needed.
    t.attach_queue(1, &mut nic.tx.lock().q, None);
    nic.has_irq.store(rx_irq, Ordering::Relaxed);
    CARDS.lock().push(nic);
    // Known to the stack before the first frame can arrive.
    let index = net::register(Arc::new(Handle(nic)));
    nic.iface.store(index, Ordering::Release);
    VirtioNet::post_rx(&mut nic.rx.lock());
    t.driver_ok();
    nic.rx.lock().q.notify(0);
    if rx_irq {
        log!("virtio-net", "interrupts via MSI-X");
    }
}

static CARDS: IrqMutex<Vec<&'static VirtioNet>> = IrqMutex::new(Vec::new());

/// After sleep: the device was reset; negotiate again and restart both queues.
pub fn resume() {
    let cards = CARDS.lock().clone();
    for n in cards {
        if n.t.negotiate(n.features, F_VERSION_1).is_none() {
            log!("virtio-net", "device did not come back after sleep");
            continue;
        }
        n.t.no_config_interrupt();
        let has_irq = n.has_irq.load(Ordering::Relaxed);
        {
            let mut rx = n.rx.lock();
            rx.q.reset();
            rx.free_slots = (0..rx.q.size as usize).rev().collect();
            rx.slot_of.iter_mut().for_each(|s| *s = None);
            n.t.attach_queue(0, &mut rx.q, has_irq.then_some(0));
            VirtioNet::post_rx(&mut rx);
        }
        {
            let mut tx = n.tx.lock();
            tx.q.reset();
            tx.free_slots = (0..tx.q.size as usize).rev().collect();
            tx.slot_of.iter_mut().for_each(|s| *s = None);
            n.t.attach_queue(1, &mut tx.q, None);
        }
        n.t.driver_ok();
        n.rx.lock().q.notify(0);
    }
}

/// The stack's handle to the (static) device.
struct Handle(&'static VirtioNet);

impl Nic for Handle {
    fn driver(&self) -> String {
        self.0.driver()
    }
    fn mac(&self) -> [u8; 6] {
        self.0.mac()
    }
    fn link_up(&self) -> bool {
        self.0.link_up()
    }
    fn send(&self, frame: &[u8]) -> bool {
        self.0.send(frame)
    }
    fn poll(&self) {
        self.0.poll()
    }
}
